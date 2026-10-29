//! Broader model-based recovery checks for the isolated EKV candidate.
use super::*;
use std::collections::BTreeMap;

type Model = BTreeMap<Vec<u8>, Vec<u8>>;
type Changes = Vec<(Vec<u8>, Option<Vec<u8>>)>;

fn changes(generation: u64) -> Changes {
    let mut changes = Vec::new();
    for index in 0..24 {
        if generation == 1 || index % 3 == generation as usize % 3 {
            let key = format!("episode/{index:02}").into_bytes();
            let value = if (generation + index as u64).is_multiple_of(7) {
                None
            } else {
                let mut value =
                    vec![index as u8; 32 + (generation as usize * 31 + index * 13) % 992];
                value[..8].copy_from_slice(&generation.to_le_bytes());
                Some(value)
            };
            changes.push((key, value));
        }
    }
    if generation == 1 {
        // A read tombstone must survive changes and compactions of unrelated keys.
        changes.push((b"read/tombstone".to_vec(), Some(b"read,reclaimed".to_vec())));
    }
    changes
}

async fn apply<F: Flash>(db: &Database<F, NoopRawMutex>, changes: &Changes) -> Result<(), String> {
    let mut tx = db.write_transaction().await;
    for (key, value) in changes {
        match value {
            Some(value) => tx.write(key, value).await,
            None => tx.delete(key).await,
        }
        .map_err(|error| format!("{error:?}"))?;
    }
    tx.commit().await.map_err(|error| format!("{error:?}"))
}

fn advance(model: &mut Model, changes: &Changes) {
    for (key, value) in changes {
        match value {
            Some(value) => {
                model.insert(key.clone(), value.clone());
            }
            None => {
                model.remove(key);
            }
        }
    }
}

fn contents<F: Flash>(db: &Database<F, NoopRawMutex>) -> Model {
    complete(async {
        let tx = db.read_transaction().await;
        let mut cursor = tx.read_all().await.unwrap();
        let (mut key, mut value) = ([0; 64], [0; 4096]);
        let mut found = Model::new();
        while let Some((k, v)) = cursor.next(&mut key, &mut value).await.unwrap() {
            assert!(
                found
                    .insert(key[..k].to_vec(), value[..v].to_vec())
                    .is_none()
            );
        }
        // Direct reads independently agree with the ordered scan, including deleted keys.
        for index in 0..24 {
            let key = format!("episode/{index:02}").into_bytes();
            match (found.get(&key), tx.read(&key, &mut value).await) {
                (Some(expected), Ok(n)) => assert_eq!(&value[..n], expected),
                (None, Err(ekv::ReadError::KeyNotFound)) => (),
                state => panic!("scan/get disagreement: {state:?}"),
            }
        }
        found
    })
}

#[derive(Clone)]
struct Checkpoint {
    bytes: Vec<u8>,
    old: Model,
    new: Model,
    changes: Changes,
    operations: Vec<Operation>,
    generation: u64,
}

fn history() -> Vec<Checkpoint> {
    let flash = Nor::new(vec![ERASE_VALUE; PAGES * PAGE_SIZE]);
    let db = Db::new(flash.clone(), Config::default());
    complete(db.format()).unwrap();
    drop(db);
    let mut bytes = flash.snapshot();
    let mut model = Model::new();
    let mut checkpoints = Vec::new();
    let mut rollovers = 0;
    let mut erased_pages = std::collections::BTreeSet::new();
    let mut reused = false;
    let mut erases = 0;
    for generation in 1..=192 {
        let (flash, db) = mounted(bytes.clone());
        let changes = changes(generation);
        let old = model.clone();
        complete(apply(&db, &changes)).unwrap();
        advance(&mut model, &changes);
        assert_eq!(contents(&db), model, "generation={generation}");
        let operations = flash.0.borrow().operations.clone();
        for operation in &operations {
            if operation.erase {
                erases += 1;
                reused |= !erased_pages.insert(operation.page.index());
            }
        }
        let rollover = operations.iter().any(|op| {
            !op.erase && op.offset == 0 && op.data.starts_with(&0x1d81bcde_u32.to_le_bytes())
        });
        if rollover {
            rollovers += 1;
        }
        if (rollover && rollovers <= 3) || [32, 96, 160].contains(&generation) {
            println!(
                "history checkpoint generation={generation} rollover={rollover} logical_ops={}",
                operations.len()
            );
            checkpoints.push(Checkpoint {
                bytes: bytes.clone(),
                old,
                new: model.clone(),
                changes,
                operations,
                generation,
            });
        }
        drop(db);
        bytes = flash.snapshot();
    }
    let (_, reopened) = mounted(bytes);
    assert_eq!(contents(&reopened), model);
    assert!(
        rollovers >= 3,
        "history must cover repeated metadata rollover"
    );
    assert!(reused, "history must reuse previously erased pages");
    println!(
        "history: 192 commits/reopens, {rollovers} metadata rollovers, {erases} erases, page reuse={reused}"
    );
    checkpoints
}

fn verify_recovery(bytes: Vec<u8>, checkpoint: &Checkpoint, context: &str) {
    let (_, db) = mounted(bytes);
    let recovered = contents(&db);
    assert!(
        recovered == checkpoint.old || recovered == checkpoint.new,
        "generation={} {context}: catalogue is neither complete old nor complete new state",
        checkpoint.generation
    );
    assert_eq!(
        recovered.get(b"read/tombstone".as_slice()).unwrap(),
        b"read,reclaimed"
    );
}

#[test]
#[ignore = "exhaustive recovery sweep: just test-storage-recovery"]
fn repeated_rollover_compaction_and_deletion_logical_cuts() {
    // Regression (t-116): full catalogue and read tombstone survive mixed histories.
    let checkpoints = history();
    let mut cases = 0;
    for checkpoint in &checkpoints {
        for (cut, operation) in checkpoint.operations.iter().enumerate() {
            for prefix in [0, operation.data.len()] {
                let (flash, db) = mounted(checkpoint.bytes.clone());
                flash.0.borrow_mut().cut = Some((cut, prefix));
                let mut future = Box::pin(apply(&db, &checkpoint.changes));
                assert!(matches!(
                    future
                        .as_mut()
                        .poll(&mut Context::from_waker(Waker::noop())),
                    Poll::Pending
                ));
                drop(future);
                drop(db);
                verify_recovery(
                    flash.snapshot(),
                    checkpoint,
                    &format!("logical cut={cut} prefix={prefix}"),
                );
                cases += 1;
                if cases % 128 == 0 {
                    println!("mixed-history logical sweep: {cases}");
                }
            }
        }
    }
    println!(
        "mixed-history logical cuts: {cases} across {} checkpoints",
        checkpoints.len()
    );
}

#[test]
#[ignore = "exhaustive recovery sweep: just test-storage-recovery"]
fn mixed_history_sector_and_sync_faults() {
    // Regression (t-116): complete model recovery after torn writes, failed barriers and cancellation.
    let checkpoints = history();
    let checkpoint = checkpoints.iter().find(|p| p.generation >= 96).unwrap();
    let make = |cut, prefix, cancel| {
        let file = SectorFault {
            bytes: Rc::new(RefCell::new(checkpoint.bytes.clone())),
            count: Rc::new(std::cell::Cell::new(0)),
            cut,
            prefix,
            cancel,
        };
        let store = complete(PageStore::open(file.clone(), PAGES)).unwrap();
        let db = Database::<_, NoopRawMutex>::new(store, Config::default());
        complete(db.mount()).unwrap();
        (file, db)
    };
    let (file, db) = make(None, 512, false);
    complete(apply(&db, &checkpoint.changes)).unwrap();
    assert_eq!(contents(&db), checkpoint.new);
    let boundaries = file.count.get();
    let mut cases = 0;
    for cancel in [false, true] {
        for prefix in [0, 217, 512] {
            for cut in 0..boundaries {
                let (file, db) = make(Some(cut), prefix, cancel);
                let mut future = Box::pin(apply(&db, &checkpoint.changes));
                let result = future
                    .as_mut()
                    .poll(&mut Context::from_waker(Waker::noop()));
                if cancel {
                    assert!(matches!(result, Poll::Pending));
                } else {
                    assert!(matches!(result, Poll::Ready(Err(_))));
                }
                drop(future);
                drop(db);
                verify_recovery(
                    file.bytes.borrow().clone(),
                    checkpoint,
                    &format!("sector cut={cut} prefix={prefix} cancel={cancel}"),
                );
                cases += 1;
                if cases % 256 == 0 {
                    println!("mixed-history sector sweep: {cases}/{}", boundaries * 6);
                }
            }
        }
    }
    println!(
        "mixed-history sector/sync cuts: {cases}, generation={}",
        checkpoint.generation
    );
}

#[test]
#[ignore = "extended recovery history: just test-storage-recovery"]
fn recovered_catalogue_can_commit_large_values_and_reopen_again() {
    // Regression (t-116): recovery must rebuild allocation state, not only permit one read.
    let checkpoints = history();
    let mut cases = 0;
    for checkpoint in &checkpoints {
        let cut = checkpoint
            .operations
            .iter()
            .position(|operation| {
                !operation.erase
                    && operation.offset == 0
                    && operation.data.starts_with(&0x1d81bcde_u32.to_le_bytes())
            })
            .unwrap_or(checkpoint.operations.len() - 1);
        for prefix in [0, checkpoint.operations[cut].data.len()] {
            let (flash, db) = mounted(checkpoint.bytes.clone());
            flash.0.borrow_mut().cut = Some((cut, prefix));
            let mut interrupted = Box::pin(apply(&db, &checkpoint.changes));
            assert!(matches!(
                interrupted
                    .as_mut()
                    .poll(&mut Context::from_waker(Waker::noop())),
                Poll::Pending
            ));
            drop(interrupted);
            drop(db);
            let (recovered_flash, recovered) = mounted(flash.snapshot());
            let mut expected = contents(&recovered);
            assert!(expected == checkpoint.old || expected == checkpoint.new);
            for generation in 0..12u8 {
                let changes = vec![(b"zz/recovery".to_vec(), Some(vec![generation; 4096]))];
                complete(apply(&recovered, &changes)).unwrap();
                advance(&mut expected, &changes);
                assert_eq!(contents(&recovered), expected);
            }
            drop(recovered);
            let (_, reopened) = mounted(recovered_flash.snapshot());
            assert_eq!(contents(&reopened), expected);
            cases += 1;
        }
    }
    println!("post-recovery large-value commits/reopen: {cases} histories x 12 commits");
}
