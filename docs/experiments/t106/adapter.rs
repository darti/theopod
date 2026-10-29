//! Disposable feasibility adapter; no firmware integration.
use crate::page_store::{PageError, PageStore, SectorFile};
use embassy_sync_06::blocking_mutex::raw::NoopRawMutex;
pub type Db<F> = ekv::Database<PageStore<F>, NoopRawMutex>;
pub enum BatchError<E> {
    Write(ekv::WriteError<PageError<E>>),
    Commit(ekv::CommitError<PageError<E>>),
}
pub async fn recover<F: SectorFile>(
    db: &Db<F>,
) -> Result<(), ekv::MountError<PageError<F::Error>>> {
    db.mount().await
}
pub async fn get<F: SectorFile>(
    db: &Db<F>,
    key: &[u8],
    out: &mut [u8],
) -> Result<usize, ekv::ReadError<PageError<F::Error>>> {
    db.read_transaction().await.read(key, out).await
}
pub async fn put<F: SectorFile>(
    db: &Db<F>,
    key: &[u8],
    value: &[u8],
) -> Result<(), BatchError<F::Error>> {
    batch(db, &[(key, Some(value))]).await
}
/// Input must already have unique keys sorted lexicographically.
pub async fn batch<F: SectorFile>(
    db: &Db<F>,
    changes: &[(&[u8], Option<&[u8]>)],
) -> Result<(), BatchError<F::Error>> {
    let mut tx = db.write_transaction().await;
    for (key, value) in changes {
        match value {
            Some(v) => tx.write(key, v).await,
            None => tx.delete(key).await,
        }
        .map_err(BatchError::Write)?;
    }
    tx.commit().await.map_err(BatchError::Commit)
}
/// Bounded output via callback; caller owns the fixed buffers.
pub async fn scan<F: SectorFile>(
    db: &Db<F>,
    key: &mut [u8],
    value: &mut [u8],
    mut emit: impl FnMut(&[u8], &[u8]),
) -> Result<(), ekv::CursorError<PageError<F::Error>>> {
    let tx = db.read_transaction().await;
    let mut cursor = tx.read_all().await.map_err(ekv::CursorError::from)?;
    while let Some((k, v)) = cursor.next(key, value).await? {
        emit(&key[..k], &value[..v]);
    }
    Ok(())
}
