//! Receipt delivery is independent of business completion. A single caller DB
//! transaction binds the immutable receipt, event and usage projection. Crashes
//! before this transaction are recovered from the meter; crashes after it replay.
use std::{error::Error,path::Path};
use rusqlite::{params,OptionalExtension,TransactionBehavior};
use serde_json::{json,Value};
use crate::{agent_core_contracts::{LocalRun,LocalRunUsage,RuntimeEvent,RuntimeEventType},store::local_runs::LocalRunLedger};

/// Persist the destination before provider execution, closing even the window
/// where the caller cannot be re-claimed after its process dies.
pub(crate) fn bind(ledger:&LocalRunLedger,root:&Path,receipt_run:&str,local_run:&str,step:&str,attempt:u32)
    -> Result<(),Box<dyn Error>> {
    ledger.get_run(local_run)?.ok_or("receipt caller missing before execution")?;
    std::fs::create_dir_all(root)?;
    if !ledger.path().is_absolute() {return Err("receipt caller ledger must be absolute".into());}
    let path=ledger.path().to_string_lossy().into_owned();
    let mut db=super::codex_usage::connection(&root.join("usage.sqlite3"))?;
    let tx=db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let prior:Option<(String,String,String,u32)>=tx.query_row("SELECT ledger,local_run,step,attempt FROM caller_bindings WHERE run=?1",[receipt_run],
        |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
    if let Some(prior)=prior {
        if prior!=(path,local_run.into(),step.into(),attempt) {return Err("provider receipt caller binding conflict".into());}
    } else {
        tx.execute("INSERT INTO caller_bindings(run,ledger,local_run,step,attempt) VALUES(?1,?2,?3,?4,?5)",params![receipt_run,path,local_run,step,attempt])?;
    }
    tx.commit()?;Ok(())
}

pub(crate) fn recover_bound(root:&Path) -> Result<usize,Box<dyn Error>> {
    if !root.join("usage.sqlite3").is_file() {return Ok(0);}
    let db=super::codex_usage::connection(&root.join("usage.sqlite3"))?;
    let bindings={
        // ACK proves event delivery, not the integrity of a mutable LocalRun
        // projection. Older/stale full-snapshot writers can damage that field
        // after ACK. Reconcile completed bindings, including delivered=1.
        let mut s=db.prepare("SELECT b.run,b.ledger,b.local_run,b.step,b.attempt FROM caller_bindings b JOIN receipts r ON r.run=b.run WHERE r.result IS NOT NULL ORDER BY b.run")?;
        let rows=s.query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,u32>(4)?)))?.collect::<Result<Vec<_>,_>>()?;
        rows
    };
    let mut count=0;
    for (provider,path,run,step,attempt) in bindings {
        if recover(&LocalRunLedger::new(path),root,&provider,&run,&step,attempt)?.is_some() {count+=1;}
    }
    Ok(count)
}

pub(crate) fn bound_caller(root:&Path,provider_run:&str,ledger:&LocalRunLedger) -> Result<Option<String>,Box<dyn Error>> {
    if !root.join("usage.sqlite3").is_file() {return Ok(None);}
    let db=super::codex_usage::connection(&root.join("usage.sqlite3"))?;
    let prior:Option<(String,String)>=db.query_row("SELECT ledger,local_run FROM caller_bindings WHERE run=?1",[provider_run],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    if let Some((path,run))=prior {
        if std::path::Path::new(&path).canonicalize()?!=ledger.path().canonicalize()? {return Err("receipt caller ledger changed".into());}
        return Ok(Some(run));
    }
    Ok(None)
}

pub(crate) fn recover(ledger:&LocalRunLedger, root:&Path, receipt_run:&str,
    local_run:&str, step:&str, attempt:u32) -> Result<Option<LocalRun>,Box<dyn Error>> {
    let Some(receipt)=super::codex_usage::read_receipt(root,receipt_run)? else {return Ok(None);};
    let run=deliver(ledger,receipt_run,local_run,step,attempt,&receipt)?;
    // An interrupted acknowledgement only causes an idempotent redelivery.
    super::codex_usage::connection(&root.join("usage.sqlite3"))?.execute(
        "UPDATE caller_bindings SET delivered=1 WHERE run=?1",[receipt_run])?;
    Ok(Some(run))
}

fn deliver(ledger:&LocalRunLedger, receipt_run:&str, local_run:&str, step:&str, attempt:u32,
    receipt:&Value) -> Result<LocalRun,Box<dyn Error>> {
    if receipt["run_id"]!=receipt_run {return Err("receipt does not belong to this provider run".into());}
    ledger.get_run(local_run)?.ok_or("receipt caller run missing")?; // initialize supported ledger schema
    let mut db=rusqlite::Connection::open(ledger.path())?;
    db.busy_timeout(std::time::Duration::from_secs(5))?;
    db.execute_batch("PRAGMA synchronous=FULL; CREATE TABLE IF NOT EXISTS codex_receipt_deliveries(
        receipt_run TEXT PRIMARY KEY,local_run TEXT NOT NULL,step TEXT NOT NULL,attempt INTEGER NOT NULL,receipt TEXT NOT NULL);")?;
    let tx=db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let serialized=serde_json::to_string(receipt)?;
    let prior:Option<(String,String,u32,String)>=tx.query_row(
        "SELECT local_run,step,attempt,receipt FROM codex_receipt_deliveries WHERE receipt_run=?1",[receipt_run],
        |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
    let payload:String=tx.query_row("SELECT payload_json FROM local_runs WHERE run_id=?1",[local_run],|r|r.get(0))?;
    let mut run:LocalRun=serde_json::from_str(&payload)?;
    if let Some(ref prior)=prior {
        if prior!=&(local_run.into(),step.into(),attempt,serialized.clone()) {return Err("receipt delivery binding conflict".into());}
    }
    let usage=super::codex_usage::increment(receipt);
    rebuild_projection(&tx,&mut run,Some((step,attempt,&usage)))?;
    if prior.is_some() {
        // Existing marker suppresses a duplicate event, never projection repair.
        tx.execute("UPDATE local_runs SET payload_json=?2 WHERE run_id=?1",params![local_run,serde_json::to_string(&run)?])?;
        tx.commit()?;
        return Ok(run);
    }
    let sequence:i64=tx.query_row("SELECT MAX(COALESCE((SELECT MAX(sequence)+1 FROM runtime_events WHERE run_id=?1),2),COALESCE((SELECT next_sequence+1 FROM runtime_sequence_allocations WHERE run_id=?1),2),2)",[local_run],|r|r.get(0))?;
    let event=RuntimeEvent {schema_version:"runtime_event.v1".into(),event_id:format!("{local_run}:codex-receipt:{receipt_run}"),
        run_id:local_run.into(),step_id:step.into(),capability_id:step.into(),sequence:sequence as u64,
        provider:"personal.codex".into(),event_type:RuntimeEventType::UsageRecorded,
        occurred_at:chrono::Utc::now().timestamp().to_string(),payload:json!({"usage_receipt":receipt,"runtime_usage":usage,
            "runtime_attempt":attempt,"receipt_recovery_only":true,"business_success":false})};
    event.validate().map_err(std::io::Error::other)?;
    tx.execute("INSERT INTO runtime_events(event_id,run_id,step_id,capability_id,sequence,provider,event_type,occurred_at,payload_json) VALUES(?1,?2,?3,?4,?5,?6,'usage_recorded',?7,?8)",
        params![event.event_id,local_run,step,step,sequence,event.provider,event.occurred_at,serde_json::to_string(&event)?])?;
    tx.execute("INSERT INTO runtime_sequence_allocations(run_id,next_sequence) VALUES(?1,?2) ON CONFLICT(run_id) DO UPDATE SET next_sequence=excluded.next_sequence",params![local_run,sequence])?;
    tx.execute("UPDATE local_runs SET payload_json=?2 WHERE run_id=?1",params![local_run,serde_json::to_string(&run)?])?;
    tx.execute("INSERT INTO codex_receipt_deliveries VALUES(?1,?2,?3,?4,?5)",params![receipt_run,local_run,step,attempt,serialized])?;
    tx.commit()?;
    Ok(run)
}

fn rebuild_projection(tx:&rusqlite::Transaction<'_>,run:&mut LocalRun,
    incoming:Option<(&str,u32,&Option<LocalRunUsage>)>) -> Result<(),Box<dyn Error>> {
    let mut rows=std::collections::BTreeMap::<(String,u64),LocalRunUsage>::new();
    {
        let mut statement=tx.prepare("SELECT payload_json FROM runtime_events WHERE run_id=?1 ORDER BY sequence")?;
        for row in statement.query_map([&run.run_id],|r|r.get::<_,String>(0))? {
            let event:RuntimeEvent=serde_json::from_str(&row?)?;
            if let Some(value)=event.payload.get("runtime_usage").filter(|v|!v.is_null()) {
                rows.insert((event.step_id,event.payload["runtime_attempt"].as_u64().unwrap_or(1)),serde_json::from_value(value.clone())?);
            }
        }
    }
    if let Some((step,attempt,Some(usage)))=incoming {rows.insert((step.into(),attempt as u64),usage.clone());}
    // No zero substitution for missing usage. Preserve comparable prices from
    // other providers; a Codex/unknown-currency row makes the aggregate unknown.
    if !rows.is_empty() {
        let first=rows.values().next().ok_or("usage projection is empty")?;
        let mut total=LocalRunUsage {input_tokens:0,output_tokens:0,estimated_cost:0.0,
            currency:first.currency.clone(),billing_owner:first.billing_owner.clone()};
        let comparable=rows.values().all(|row|!row.currency.trim().is_empty() && row.currency==first.currency
            && row.estimated_cost.is_finite() && row.estimated_cost>=0.0);
        for row in rows.values() {
            total.input_tokens=total.input_tokens.checked_add(row.input_tokens).ok_or("usage input overflow")?;
            total.output_tokens=total.output_tokens.checked_add(row.output_tokens).ok_or("usage output overflow")?;
            if comparable {total.estimated_cost+=row.estimated_cost;}
            if row.billing_owner!=total.billing_owner {total.billing_owner="mixed".into();}
        }
        if !comparable {total.currency.clear();total.estimated_cost=0.0;}
        if !total.estimated_cost.is_finite() {return Err("usage cost overflow".into());}
        run.usage=Some(total);
    }
    Ok(())
}

// Keep terminal state and its current usage export in the same transaction.
// A later no-op cancel never has to repair an earlier export. Replaying an
// identical projection preserves its bytes, delivery status and attempts.
fn project_in_transaction(tx:&rusqlite::Transaction<'_>,run:&LocalRun)->Result<(),Box<dyn Error>> {
    let interaction:String=tx.query_row("SELECT payload_json FROM local_interactions WHERE interaction_id=?1",[&run.interaction_id],|r|r.get(0))?;
    let key=format!("projection:{}:{}",run.run_id,run.updated_at);
    let prior:Option<String>=tx.query_row("SELECT payload_json FROM projection_outbox WHERE dedupe_key=?1",[&key],|r|r.get(0)).optional()?;
    if let Some(ref prior)=prior {
        let value:Value=serde_json::from_str(prior)?;
        if value["run"]==serde_json::to_value(run)? {return Ok(());}
    }
    let now=chrono::Utc::now().timestamp().to_string();
    let projection=crate::agent_core_contracts::RunProjection {
        schema_version:crate::agent_core_contracts::RUN_PROJECTION_SCHEMA_VERSION.into(),
        projection_id:format!("projection_{}_{}",run.run_id,run.updated_at),idempotency_key:key.clone(),sent_at:now.clone(),
        interaction:serde_json::from_str(&interaction)?,run:run.clone()};
    projection.validate().map_err(std::io::Error::other)?;
    tx.execute("INSERT INTO projection_outbox(projection_type,aggregate_id,dedupe_key,payload_json,status,attempts,next_attempt_at,last_error,created_at,updated_at)
        VALUES('run_projection',?1,?2,?3,'pending',0,'','',?4,?4)
        ON CONFLICT(dedupe_key) DO UPDATE SET payload_json=excluded.payload_json,status='pending',attempts=0,next_attempt_at='',last_error='',updated_at=excluded.updated_at",
        params![run.run_id,key,serde_json::to_string(&projection)?,now])?;
    Ok(())
}

/// Serialize caller state changes with receipt delivery. The caller supplies an
/// intent, not a stale full snapshot: load the current state under the same
/// IMMEDIATE transaction as the write. LocalRunLedger stays outside this patch.
pub(crate) fn update_caller(ledger:&LocalRunLedger,run_id:&str,change:impl FnOnce(&mut LocalRun)->bool)
    -> Result<(LocalRun,bool),Box<dyn Error>> {
    ledger.get_run(run_id)?.ok_or("receipt caller run missing")?;
    let mut db=rusqlite::Connection::open(ledger.path())?;
    db.busy_timeout(std::time::Duration::from_secs(5))?;
    db.execute_batch("PRAGMA synchronous=FULL;")?;
    let tx=db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let payload:String=tx.query_row("SELECT payload_json FROM local_runs WHERE run_id=?1",[run_id],|r|r.get(0))?;
    let mut run:LocalRun=serde_json::from_str(&payload)?;
    if !change(&mut run) {
        // A latest-state no-op must not repair/rewrite the payload or requeue
        // projections. Receipt recovery is a separate explicit operation.
        tx.commit()?;
        return Ok((run,false));
    }
    rebuild_projection(&tx,&mut run,None)?;
    run.validate().map_err(std::io::Error::other)?;
    tx.execute("UPDATE local_runs SET status=?2,current_step_id=?3,payload_json=?4,updated_at=?5 WHERE run_id=?1",
        params![run_id,serde_json::to_value(&run.status)?.as_str().ok_or("invalid caller status")?,
            run.current_step_id,serde_json::to_string(&run)?,run.updated_at])?;
    project_in_transaction(&tx,&run)?;
    tx.commit()?;
    Ok((run,true))
}

/// Commit paid work independently of business validation. The attempt event
/// and its projection share one durable transaction, including late delivery
/// after cancellation. No caller snapshot can erase another committed attempt.
pub(crate) fn publish_attempt_usage(ledger:&LocalRunLedger,run_id:&str,step:&str,attempt:u32,
    usage:&LocalRunUsage) -> Result<LocalRun,Box<dyn Error>> {
    ledger.get_run(run_id)?.ok_or("usage caller run missing")?;
    let mut db=rusqlite::Connection::open(ledger.path())?;
    db.busy_timeout(std::time::Duration::from_secs(5))?;
    db.execute_batch("PRAGMA synchronous=FULL;")?;
    let tx=db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let payload:String=tx.query_row("SELECT payload_json FROM local_runs WHERE run_id=?1",[run_id],|r|r.get(0))?;
    let mut run:LocalRun=serde_json::from_str(&payload)?;
    let existing={
        let mut statement=tx.prepare("SELECT payload_json FROM runtime_events WHERE run_id=?1 AND step_id=?2 ORDER BY sequence")?;
        let events=statement.query_map(params![run_id,step],|r|r.get::<_,String>(0))?.collect::<Result<Vec<_>,_>>()?;
        let mut found=false;
        for event in events {
            let event:RuntimeEvent=serde_json::from_str(&event)?;
            if event.payload["runtime_attempt"].as_u64().unwrap_or(1)==attempt as u64 {
                if let Some(prior)=event.payload.get("runtime_usage").filter(|v|!v.is_null()) {
                    let prior:LocalRunUsage=serde_json::from_value(prior.clone())?;
                    if &prior!=usage {return Err("immutable attempt usage conflicts with committed work".into());}
                    found=true;
                }
            }
        }
        found
    };
    rebuild_projection(&tx,&mut run,Some((step,attempt,&Some(usage.clone()))))?;
    run.validate().map_err(std::io::Error::other)?;
    tx.execute("UPDATE local_runs SET payload_json=?2 WHERE run_id=?1",params![run_id,serde_json::to_string(&run)?])?;
    #[cfg(test)]
    BEFORE_ATTEMPT_EVENT.with(|hook| {if let Some(hook)=hook.borrow_mut().take() {hook(&run);}});
    if !existing {
        let sequence:i64=tx.query_row("SELECT MAX(COALESCE((SELECT MAX(sequence)+1 FROM runtime_events WHERE run_id=?1),2),COALESCE((SELECT next_sequence+1 FROM runtime_sequence_allocations WHERE run_id=?1),2),2)",[run_id],|r|r.get(0))?;
        let event=RuntimeEvent {schema_version:"runtime_event.v1".into(),event_id:format!("{run_id}:attempt-usage:{step}:{attempt}"),
            run_id:run_id.into(),step_id:step.into(),capability_id:step.into(),sequence:sequence as u64,
            provider:run.runtime_provider.clone(),event_type:RuntimeEventType::UsageRecorded,
            occurred_at:chrono::Utc::now().timestamp().to_string(),
            payload:json!({"runtime_usage":usage,"runtime_attempt":attempt,"business_success":false})};
        event.validate().map_err(std::io::Error::other)?;
        tx.execute("INSERT INTO runtime_events(event_id,run_id,step_id,capability_id,sequence,provider,event_type,occurred_at,payload_json) VALUES(?1,?2,?3,?3,?4,?5,'usage_recorded',?6,?7)",
            params![event.event_id,run_id,step,sequence,event.provider,event.occurred_at,serde_json::to_string(&event)?])?;
        tx.execute("INSERT INTO runtime_sequence_allocations(run_id,next_sequence) VALUES(?1,?2) ON CONFLICT(run_id) DO UPDATE SET next_sequence=excluded.next_sequence",params![run_id,sequence])?;
    }
    if run.status.is_terminal() {project_in_transaction(&tx,&run)?;}
    tx.commit()?;
    #[cfg(test)]
    AFTER_ATTEMPT_COMMIT.with(|hook| {if let Some(hook)=hook.borrow_mut().take() {hook();}});
    Ok(run)
}

#[cfg(test)]
pub(crate) fn fail_after_receipt_commit() -> Result<(),Box<dyn Error>> {
    if AFTER_COMMIT.with(|v|v.replace(false)) {return Err("injected crash after receipt commit before caller ledger".into());}
    Ok(())
}
#[cfg(test)]
thread_local! {pub(crate) static AFTER_COMMIT:std::cell::Cell<bool>=const {std::cell::Cell::new(false)};}
#[cfg(test)]
thread_local! {pub(crate) static BEFORE_ATTEMPT_EVENT:std::cell::RefCell<Option<Box<dyn FnOnce(&LocalRun)>>>=const {std::cell::RefCell::new(None)};}
#[cfg(test)]
thread_local! {pub(crate) static AFTER_ATTEMPT_COMMIT:std::cell::RefCell<Option<Box<dyn FnOnce()>>>=const {std::cell::RefCell::new(None)};}
