//! Durable provider counters are observations. Only a run-bound, known baseline
//! may become billable token deltas. File locks cover the actual provider turn;
//! SQLite IMMEDIATE transactions make reservations/receipts crash persistent.
use fs4::fs_std::FileExt;
use rusqlite::{params, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    error::Error,
    fs::{File, OpenOptions},
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct Counters {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_input_tokens: Option<u64>,
    pub reasoning_output_tokens: Option<u64>,
}

pub(crate) struct Meter {
    db: PathBuf,
    run: String,
    resume: String,
    _lock: File,
}
pub(super) fn connection(path: &Path) -> Result<rusqlite::Connection, Box<dyn Error>> {
    let db = rusqlite::Connection::open(path)?;
    db.busy_timeout(std::time::Duration::from_secs(5))?;
    db.execute_batch("PRAGMA synchronous=FULL;
        BEGIN IMMEDIATE;
        CREATE TABLE IF NOT EXISTS sessions(id TEXT PRIMARY KEY, raw TEXT, pending TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS receipts(run TEXT PRIMARY KEY, resume TEXT NOT NULL, baseline TEXT, result TEXT);
        CREATE TABLE IF NOT EXISTS caller_bindings(run TEXT PRIMARY KEY,ledger TEXT NOT NULL,local_run TEXT NOT NULL,
            step TEXT NOT NULL,attempt INTEGER NOT NULL,delivered INTEGER NOT NULL DEFAULT 0);
        COMMIT;")?;
    Ok(db)
}

impl Meter {
    pub(crate) fn begin(root: &Path, run: &str, resume: &str) -> Result<Self, Box<dyn Error>> {
        if run.is_empty() {
            return Err("Codex usage requires a durable run identity".into());
        }
        std::fs::create_dir_all(root)?;
        let key = if resume.is_empty() {
            format!("run:{run}")
        } else {
            format!("session:{resume}")
        };
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(root.join(format!("{:x}.lock", Sha256::digest(key))))?;
        if !FileExt::try_lock_exclusive(&lock)? {
            return Err("Codex provider session already has an active turn".into());
        }
        let path = root.join("usage.sqlite3");
        let mut db = connection(&path)?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let prior: Option<(String, Option<String>)> = tx
            .query_row(
                "SELECT resume,result FROM receipts WHERE run=?1",
                [run],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if let Some((prior, result)) = prior {
            if prior != resume {
                return Err("Codex usage run/session binding conflict".into());
            }
            // A released process lock does not prove that an interrupted turn
            // never reached the provider. Never reuse that reservation/baseline,
            // even if no newer turn has settled yet. A retry needs a new run.
            if result.is_none() {
                return Err(
                    "Codex unsettled run identity cannot be reentered; use a new run identity"
                        .into(),
                );
            }
        } else {
            let baseline: Option<String> = if resume.is_empty() {
                Some(serde_json::to_string(&Counters {
                    input_tokens: 0,
                    output_tokens: 0,
                    cached_input_tokens: Some(0),
                    reasoning_output_tokens: Some(0),
                })?)
            } else {
                tx.query_row("SELECT raw FROM sessions WHERE id=?1", [resume], |r| {
                    r.get(0)
                })
                .optional()?
                .flatten()
            };
            tx.execute(
                "INSERT INTO receipts(run,resume,baseline) VALUES(?1,?2,?3)",
                params![run, resume, baseline],
            )?;
        }
        // Invalidate the public baseline before execution. If the worker dies
        // without counters, a different run cannot charge unobserved history.
        let finished: Option<String> =
            tx.query_row("SELECT result FROM receipts WHERE run=?1", [run], |r| {
                r.get(0)
            })?;
        if !resume.is_empty() && finished.is_none() {
            tx.execute(
                "INSERT INTO sessions(id,raw,pending) VALUES(?1,NULL,?2)
                ON CONFLICT(id) DO UPDATE SET raw=NULL,pending=excluded.pending",
                params![resume, run],
            )?;
        }
        tx.commit()?;
        Ok(Self {
            db: path,
            run: run.into(),
            resume: resume.into(),
            _lock: lock,
        })
    }

    pub(crate) fn completed(&self) -> Result<bool, Box<dyn Error>> {
        let result: Option<String> = connection(&self.db)?.query_row(
            "SELECT result FROM receipts WHERE run=?1",
            [&self.run],
            |r| r.get(0),
        )?;
        Ok(result.is_some())
    }

    pub(crate) fn receipt(&self) -> Result<Option<Value>, Box<dyn Error>> {
        read_receipt(&self.db.parent().ok_or("usage root missing")?, &self.run)
    }

    pub(crate) fn settle(
        &self,
        session: &str,
        raw: Option<&Counters>,
    ) -> Result<Value, Box<dyn Error>> {
        if !self.resume.is_empty() && session != self.resume && !session.is_empty() {
            return Err("Codex returned a different resumed provider session".into());
        }
        let mut db = connection(&self.db)?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (baseline, prior): (Option<String>, Option<String>) = tx.query_row(
            "SELECT baseline,result FROM receipts WHERE run=?1",
            [&self.run],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if let Some(prior) = prior {
            let prior: Value = serde_json::from_str(&prior)?;
            if prior["provider_raw"] != serde_json::to_value(raw)?
                || prior["provider_session_id"] != session
            {
                return Err("Codex usage replay conflicts with immutable receipt".into());
            }
            return Ok(prior);
        }
        if !self.resume.is_empty() {
            let pending: String = tx.query_row(
                "SELECT pending FROM sessions WHERE id=?1",
                [&self.resume],
                |r| r.get(0),
            )?;
            if pending != self.run {
                return Err("Codex pending usage reservation was superseded".into());
            }
        } else if !session.is_empty() {
            let exists: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM sessions WHERE id=?1)",
                [session],
                |r| r.get(0),
            )?;
            if exists {
                return Err("Codex launch returned an already measured provider session".into());
            }
        }
        let baseline: Option<Counters> = baseline.map(|s| serde_json::from_str(&s)).transpose()?;
        let valid_subsets = raw.is_none_or(|r| {
            r.cached_input_tokens.is_none_or(|n| n <= r.input_tokens)
                && r.reasoning_output_tokens
                    .is_none_or(|n| n <= r.output_tokens)
        });
        let reset = raw.zip(baseline.as_ref()).is_some_and(|(r, b)| {
            r.input_tokens < b.input_tokens
                || r.output_tokens < b.output_tokens
                || r.cached_input_tokens
                    .zip(b.cached_input_tokens)
                    .is_some_and(|(r, b)| r < b)
                || r.reasoning_output_tokens
                    .zip(b.reasoning_output_tokens)
                    .is_some_and(|(r, b)| r < b)
        });
        let delta = raw.zip(baseline.as_ref()).filter(|_| !reset && valid_subsets && !session.is_empty()).map(|(r,b)|json!({
            "input_tokens":r.input_tokens-b.input_tokens,"output_tokens":r.output_tokens-b.output_tokens,
            "cached_input_tokens":r.cached_input_tokens.zip(b.cached_input_tokens).map(|(r,b)|r-b).filter(|n|*n<=r.input_tokens-b.input_tokens),
            "reasoning_output_tokens":r.reasoning_output_tokens.zip(b.reasoning_output_tokens).map(|(r,b)|r-b).filter(|n|*n<=r.output_tokens-b.output_tokens)
        }));
        let receipt = json!({"schema_version":"codex_usage_receipt.v1","run_id":self.run,
            "provider_session_id":session,"counter_scope":"provider_session_cumulative",
            "provider_raw":raw,"baseline":baseline,"run_increment":delta,
            "increment_status":if raw.is_none(){"unreported"}else if session.is_empty(){"unbound_session"}else if !valid_subsets{"invalid_subsets"}else if reset{"counter_reset_unknown"}else if baseline.is_none(){"unknown_baseline"}else{"known"},
            "cost":null,"cost_status":"unknown","cached_reasoning_semantics":"subsets_not_additional_tokens"});
        tx.execute(
            "UPDATE receipts SET result=?2 WHERE run=?1",
            params![self.run, serde_json::to_string(&receipt)?],
        )?;
        if !session.is_empty() {
            tx.execute(
                "INSERT INTO sessions(id,raw,pending) VALUES(?1,?2,'')
                ON CONFLICT(id) DO UPDATE SET raw=excluded.raw,pending=''",
                params![
                    session,
                    if valid_subsets {
                        raw.map(serde_json::to_string).transpose()?
                    } else {
                        None
                    }
                ],
            )?;
        }
        tx.commit()?;
        Ok(receipt)
    }
}

/// Immutable lookup does not reserve a session or change its pending owner.
/// Missing/unsettled usage remains unknown. Never reconstruct from provider
/// files or a mutable current-session counter.
pub(crate) fn read_receipt(root: &Path, run: &str) -> Result<Option<Value>, Box<dyn Error>> {
    let path = root.join("usage.sqlite3");
    if !path.is_file() {
        return Ok(None);
    }
    let db = connection(&path)?;
    let result: Option<String> = db
        .query_row("SELECT result FROM receipts WHERE run=?1", [run], |r| {
            r.get(0)
        })
        .optional()?
        .flatten();
    let receipt: Option<Value> = result.map(|v| serde_json::from_str(&v)).transpose()?;
    if receipt.as_ref().is_some_and(|v| v["run_id"] != run) {
        return Err("usage receipt identity conflict".into());
    }
    Ok(receipt)
}

pub(crate) fn default_root() -> PathBuf {
    crate::store::paths::agent_home().join("cache/codex-usage")
}

pub(crate) fn increment(receipt: &Value) -> Option<crate::agent_core_contracts::LocalRunUsage> {
    let value = receipt.get("run_increment")?.as_object()?;
    Some(crate::agent_core_contracts::LocalRunUsage {
        input_tokens: value.get("input_tokens")?.as_u64()?,
        output_tokens: value.get("output_tokens")?.as_u64()?,
        estimated_cost: 0.0,
        currency: String::new(),
        billing_owner: "user".into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn root() -> PathBuf {
        let root = std::env::temp_dir().join(format!("codex-usage-{:016x}", rand::random::<u64>()));
        std::fs::create_dir_all(&root).unwrap();
        root
    }
    fn counters(i: u64, o: u64) -> Counters {
        Counters {
            input_tokens: i,
            output_tokens: o,
            cached_input_tokens: Some(i / 2),
            reasoning_output_tokens: Some(o / 2),
        }
    }

    #[test]
    fn usage_launch_resume_fixed_cumulative_fixture_and_replay() {
        let root = root();
        let first = Meter::begin(&root, "launch", "").unwrap();
        let a = first
            .settle("session", Some(&counters(47224, 128)))
            .unwrap();
        assert_eq!(a["run_increment"]["input_tokens"], 47224);
        drop(first);
        let second = Meter::begin(&root, "resume", "session").unwrap();
        let b = second
            .settle("session", Some(&counters(95295, 480)))
            .unwrap();
        assert_eq!(b["run_increment"]["input_tokens"], 48071);
        assert_eq!(b["run_increment"]["output_tokens"], 352);
        assert_eq!(
            second
                .settle("session", Some(&counters(95295, 480)))
                .unwrap(),
            b
        );
        assert!(second
            .settle("session", Some(&counters(95296, 480)))
            .is_err());
        assert!(b["cost"].is_null());
        assert!(b["run_increment"]["cached_input_tokens"].as_u64().unwrap() <= 48071);
        assert!(
            b["run_increment"]["reasoning_output_tokens"]
                .as_u64()
                .unwrap()
                <= 352
        );
        drop(second);
        let replay = Meter::begin(&root, "resume", "session").unwrap();
        assert!(replay.completed().unwrap());
        assert_eq!(
            replay
                .settle("session", Some(&counters(95295, 480)))
                .unwrap(),
            b
        );
        drop(replay);
        let third = Meter::begin(&root, "next", "session")
            .unwrap()
            .settle("session", Some(&counters(95307, 489)))
            .unwrap();
        assert_eq!(third["run_increment"]["input_tokens"], 12);
        println!("contract_fixture_not_live: {b}");
    }

    #[test]
    fn usage_unknown_baseline_reset_and_subset_failures_never_become_zero() {
        let root = root();
        let unknown = Meter::begin(&root, "external", "external-session")
            .unwrap()
            .settle("external-session", Some(&counters(913, 87)))
            .unwrap();
        assert_eq!(unknown["increment_status"], "unknown_baseline");
        assert!(increment(&unknown).is_none());
        let reset = Meter::begin(&root, "reset", "external-session")
            .unwrap()
            .settle("external-session", Some(&counters(19, 3)))
            .unwrap();
        assert_eq!(reset["increment_status"], "counter_reset_unknown");
        assert!(increment(&reset).is_none());
        let after = Meter::begin(&root, "after-reset", "external-session")
            .unwrap()
            .settle("external-session", Some(&counters(37, 9)))
            .unwrap();
        assert_eq!(after["run_increment"]["input_tokens"], 18);
        let mut bad = counters(50, 10);
        bad.cached_input_tokens = Some(51);
        let invalid = Meter::begin(&root, "bad", "external-session")
            .unwrap()
            .settle("external-session", Some(&bad))
            .unwrap();
        assert_eq!(invalid["increment_status"], "invalid_subsets");
        assert!(increment(&invalid).is_none());
        assert!(unknown["provider_raw"]["input_tokens"] == 913);
    }

    #[test]
    fn usage_concurrent_resume_reservation_is_atomic_and_session_bound() {
        let root = root();
        Meter::begin(&root, "initial", "")
            .unwrap()
            .settle("s", Some(&counters(100, 21)))
            .unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
        let release = std::sync::Arc::new(std::sync::Barrier::new(3));
        let results = std::thread::scope(|scope| {
            let handles = (0..2)
                .map(|i| {
                    let root = &root;
                    let barrier = &barrier;
                    let release = &release;
                    scope.spawn(move || {
                        barrier.wait();
                        let meter = Meter::begin(root, &format!("competing-{i}"), "s");
                        release.wait();
                        meter
                            .map(|m| m.settle("s", Some(&counters(141, 28))).unwrap())
                            .ok()
                    })
                })
                .collect::<Vec<_>>();
            barrier.wait();
            release.wait();
            handles
                .into_iter()
                .map(|h| h.join().unwrap())
                .collect::<Vec<_>>()
        });
        assert_eq!(results.iter().filter(|r| r.is_some()).count(), 1);
        let receipt = results.into_iter().flatten().next().unwrap();
        assert_eq!(receipt["run_increment"]["input_tokens"], 41);
        let next = Meter::begin(&root, "next", "s").unwrap();
        assert!(next.settle("other", Some(&counters(170, 35))).is_err());
    }

    #[test]
    fn usage_concurrent_cold_open_initializes_receipt_schema_before_readers() {
        let root = root();
        let path = root.join("usage.sqlite3");
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(9));
        let results = std::thread::scope(|scope| {
            let handles=(0..8).map(|_| {
                let root=&root;
                let path=&path;
                let barrier=barrier.clone();
                scope.spawn(move || {
                    barrier.wait();
                    let db=connection(path).map_err(|error|error.to_string())?;
                    let exists:bool=db.query_row(
                        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='receipts')",
                        [],|row|row.get(0),
                    ).map_err(|error|error.to_string())?;
                    if !exists {return Err("receipt schema was not initialized".to_string());}
                    drop(db);
                    read_receipt(root,"not-created").map(|receipt|receipt.is_none()).map_err(|error|error.to_string())
                })
            }).collect::<Vec<_>>();
            barrier.wait();
            handles
                .into_iter()
                .map(|handle| handle.join().unwrap())
                .collect::<Vec<_>>()
        });
        assert!(
            results.iter().all(|result| matches!(result, Ok(true))),
            "{results:?}"
        );
    }

    #[test]
    fn usage_abandoned_turn_invalidates_baseline_after_restart() {
        let root = root();
        Meter::begin(&root, "initial", "")
            .unwrap()
            .settle("s", Some(&counters(77, 13)))
            .unwrap();
        drop(Meter::begin(&root, "interrupted", "s").unwrap());
        let result = Meter::begin(&root, "after-crash", "s")
            .unwrap()
            .settle("s", Some(&counters(190, 55)))
            .unwrap();
        assert_eq!(result["increment_status"], "unknown_baseline");
        assert!(increment(&result).is_none());
    }

    #[test]
    fn usage_exact_abc_crash_reentry_cannot_reuse_baseline_or_replace_pending() {
        let root = root();
        let l = Meter::begin(&root, "L", "")
            .unwrap()
            .settle("s", Some(&counters(100, 10)))
            .unwrap();
        drop(Meter::begin(&root, "A", "s").unwrap()); // crashed before settle
        assert!(Meter::begin(&root, "A", "s").is_err());
        let b = Meter::begin(&root, "B", "s")
            .unwrap()
            .settle("s", Some(&counters(200, 20)))
            .unwrap();
        assert_eq!(b["increment_status"], "unknown_baseline");
        assert!(increment(&b).is_none());
        let c = Meter::begin(&root, "C", "s")
            .unwrap()
            .settle("s", Some(&counters(230, 23)))
            .unwrap();
        assert_eq!(c["run_increment"]["input_tokens"], 30);
        assert_eq!(c["run_increment"]["output_tokens"], 3);
        let old_a =
            Meter::begin(&root, "A", "s").and_then(|old| old.settle("s", Some(&counters(250, 25))));
        assert!(old_a.is_err()); // cannot settle 250/25 or reuse 100/10
        assert!(read_receipt(&root, "A").unwrap().is_none());
        let pending = Meter::begin(&root, "D", "s").unwrap();
        assert!(Meter::begin(&root, "A", "s").is_err());
        let d = pending.settle("s", Some(&counters(250, 25))).unwrap();
        assert_eq!(d["run_increment"]["input_tokens"], 20);
        let known = [l.clone(), c.clone(), d.clone()]
            .iter()
            .map(|v| increment(v).unwrap().input_tokens)
            .sum::<u64>();
        let outputs = [l, c, d]
            .iter()
            .map(|v| increment(v).unwrap().output_tokens)
            .sum::<u64>();
        assert_eq!((known, outputs), (150, 15));
        assert_ne!((known, outputs), (280, 28));
        assert_eq!(
            read_receipt(&root, "C").unwrap().unwrap()["run_increment"]["input_tokens"],
            30
        );
    }

    #[test]
    fn usage_process_restart_preserves_baseline_and_immutable_receipt() {
        let root = root();
        Meter::begin(&root, "before", "")
            .unwrap()
            .settle("s", Some(&counters(301, 29)))
            .unwrap();
        let output = crate::runtime::process::hidden_command(std::env::current_exe().unwrap())
            .args([
                "runtime::codex_usage::tests::usage_restart_worker",
                "--exact",
                "--ignored",
                "--nocapture",
            ])
            .env("CODEX_USAGE_RESTART_FIXTURE", &root)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed; 0 failed; 0 ignored"));
        let replay = Meter::begin(&root, "child", "s").unwrap();
        assert!(replay.completed().unwrap());
        let receipt = replay.settle("s", Some(&counters(517, 62))).unwrap();
        assert_eq!(receipt["run_increment"]["input_tokens"], 216);
        assert_eq!(receipt["run_increment"]["output_tokens"], 33);
        println!("restart_receipt: {receipt}");
    }

    #[test]
    #[ignore = "invoked in a separate process by the restart contract"]
    fn usage_restart_worker() {
        let root = PathBuf::from(
            std::env::var_os("CODEX_USAGE_RESTART_FIXTURE").expect("restart fixture"),
        );
        let meter = Meter::begin(&root, "child", "s").unwrap();
        let receipt = meter.settle("s", Some(&counters(517, 62))).unwrap();
        assert_eq!(receipt["run_increment"]["input_tokens"], 216);
    }
}
