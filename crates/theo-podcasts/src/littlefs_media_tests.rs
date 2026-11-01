use super::*;
use futures::executor::block_on;
use littlefs_rust::{Config, RamStorage};
#[test]
fn real_littlefs_temp_publication_and_id_scan_preserve_logs() {
    // Regression (t-110): production file adapter publishes durable ID-only files beside the existing logs.
    block_on(async {
        let config = Config::new(4096, 8192);
        let mut storage = RamStorage::new(4096, 8192);
        Filesystem::format(&mut storage, &config).unwrap();
        let fs = Rc::new(
            Filesystem::mount(storage, config)
                .map_err(|(e, _)| e)
                .unwrap(),
        );
        fs.write_file("/theo.log", b"log").unwrap();
        let mut media = LittlefsMedia::new(fs.clone(), 32 * 1024 * 1024);
        for id in [EpisodeId(3), EpisodeId(1)] {
            media.reset_temp(id).await.unwrap();
            media.append_temp(id, b"audio").await.unwrap();
            assert_eq!(media.sync_temp(id).await.unwrap(), 5);
            media.publish_temp(id).await.unwrap();
        }
        media.reset_temp(EpisodeId(1)).await.unwrap();
        let mut ids = [EpisodeId(0); FILE_PAGE];
        let n = media.list_ids(None, &mut ids).await.unwrap();
        assert_eq!(&ids[..n], &[EpisodeId(1), EpisodeId(3)]);
        assert_eq!(media.audio_len(EpisodeId(1)).await.unwrap(), Some(5));
        media.unlink_temp(EpisodeId(1)).await.unwrap();
        media.unlink_audio(EpisodeId(1)).await.unwrap();
        media.unlink_audio(EpisodeId(1)).await.unwrap();
        assert_eq!(fs.read_to_vec("/theo.log").unwrap(), b"log");
        assert!(media.space().await.unwrap().free < 32 * 1024 * 1024);
    });
}
