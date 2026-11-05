//! Rig spike 4: prove or break the unproven items. Throwaway; nothing here is a library choice.
mod driver;
mod hooks;
mod hosts;

use driver::{Driver, End, Kill};
use hosts::{Journal, SideEffects};
use rig_agent::run::{AgentRun, RunSpec};
use rig_core::test_utils::{MockCompletionModel, MockStreamEvent as E};

fn spec(max_turns: usize) -> RunSpec {
    let mut s = RunSpec::new();
    s.max_turns = Some(max_turns);
    s
}

/// Stream script: turn 1 = one streamed tool call `add`, turn 2 = text.
fn model_add() -> MockCompletionModel {
    MockCompletionModel::from_stream_turns([
        vec![
            E::tool_call_name_delta("t1", "add"),
            E::tool_call_arguments_delta("t1", "{\"x\":1,"),
            E::tool_call_arguments_delta("t1", "\"y\":2}"),
            E::tool_call_end("t1"),
            E::final_response_with_total_tokens(3),
        ],
        vec![E::text("the answer is "), E::text("3"), E::final_response_with_total_tokens(1)],
    ])
}

/// Turn 1 = three tool calls in one turn (a, b, c), turn 2 = text.
fn model_three() -> MockCompletionModel {
    MockCompletionModel::from_stream_turns([
        vec![
            E::tool_call("ca", "effect", serde_json::json!({"tag":"A"})),
            E::tool_call("cb", "effect", serde_json::json!({"tag":"B"})),
            E::tool_call("cc", "effect", serde_json::json!({"tag":"C"})),
            E::final_response_with_total_tokens(3),
        ],
        vec![E::text("all done"), E::final_response_with_total_tokens(1)],
    ])
}

fn p(label: &str, s: impl std::fmt::Display) {
    println!("{label}: {s}");
}

fn short<T: std::fmt::Debug>(t: &T) -> String {
    format!("{t:?}").chars().take(40).collect()
}

async fn item1() {
    println!("\n=== ITEM 1: sans-I/O streaming, cut mid-stream, persist, resume ===");
    let mut host = SideEffects::new();
    // Control: uninterrupted drive.
    let mut d = Driver::new(model_add(), spec(3), &mut host);
    let mut run = AgentRun::new("add 1 and 2").max_turns(3);
    let end = d.drive(&mut run, &Kill::default()).await;
    p("1 control end", format!("{end:?}"));
    p("1 control usage", format!("{:?}", run.usage()));
    p("1 control msgs", run.full_history().len());
    let control_hist = serde_json::to_string(&run.full_history()).unwrap();

    // Cut after 2 stream items of turn 1 (name delta + first args delta): mid tool-call.
    let mut host = SideEffects::new();
    let mut d = Driver::new(model_add(), spec(3), &mut host);
    let mut run = AgentRun::new("add 1 and 2").max_turns(3);
    let end = d.drive(&mut run, &Kill { after_stream_items: Some(2), ..Default::default() }).await;
    p("1 cut end", format!("{end:?}"));
    p("1 cut trace", format!("{:?}", d.trace.last()));
    p("1 run turn after cut", run.turn());
    p("1 run usage after cut", format!("{:?}", run.usage()));
    p("1 completion_calls after cut", run.completion_calls().len());
    let saved = serde_json::to_string(&run).unwrap();
    p("1 persisted bytes", saved.len());
    drop(run);
    // Resume in a "new process": fresh model (new connection), fresh host, reloaded run.
    let mut run: AgentRun = serde_json::from_str(&saved).expect("reload");
    let mut host2 = SideEffects::new();
    let mut d2 = Driver::new(model_add(), spec(3), &mut host2);
    let end = d2.drive(&mut run, &Kill::default()).await;
    p("1 resumed end", format!("{end:?}"));
    p("1 resumed turn counter", run.turn());
    p("1 history identical to control", serde_json::to_string(&run.full_history()).unwrap() == control_hist);
    p("1 usage after resume", format!("{:?}", run.usage()));

    let mut host = SideEffects::new();
    let mut d = Driver::new(model_add(), spec(3), &mut host);
    let mut run = AgentRun::new("add 1 and 2").max_turns(3);
    let end = d.drive(&mut run, &Kill { after_stream_items: Some(4), ..Default::default() }).await;
    p("1 cut@4 end", format!("{end:?}"));
    let a = run.next_step().unwrap();
    let b = run.next_step().unwrap();
    p("1 re-issue after cut", format!("{} then {} (turn {})", short(&a), short(&b), run.turn()));
}


fn req_summary(m: &MockCompletionModel) -> Vec<String> {
    m.requests()
        .iter()
        .map(|r| {
            let msgs: Vec<String> = r
                .chat_history
                .iter()
                .map(|m| {
                    let t = format!("{m:?}");
                    let role = if t.starts_with("User") { "U" } else if t.starts_with("Assistant") { "A" } else { "?" };
                    let mut txt = String::new();
                    if t.contains("first question") { txt.push_str("[Q]"); }
                    if t.contains("INJECTED") { txt.push_str("[INJECTED]"); }
                    format!("{role}{txt}")
                })
                .collect();
            msgs.join(",")
        })
        .collect()
}

fn model_inject() -> MockCompletionModel {
    MockCompletionModel::from_stream_turns([
        // attempt 1: aborted by the host after 2 items
        vec![E::text("Let me "), E::text("answer the first question"), E::final_response_with_total_tokens(5)],
        // attempt 2 (re-issue with the message patched in): a tool call
        vec![E::tool_call("c9", "effect", serde_json::json!({"tag":"X"})), E::final_response_with_total_tokens(2)],
        // turn 2
        vec![E::text("done"), E::final_response_with_total_tokens(1)],
    ])
}

pub async fn item2() {
    use rig_core::message::Message;
    let tag = if cfg!(feature = "inject") { "VENDORED inject_message" } else { "STOCK (RequestPatch.history)" };
    println!("
=== ITEM 2: mid-stream injection -- {tag} ===");
    let model = model_inject();
    let probe = model.clone();
    let mut host = SideEffects::new();
    let mut d = Driver::new(model, spec(4), &mut host);
    d.inject = Some((2, Message::user("INJECTED: actually also consider the budget")));
    d.use_run_inject = cfg!(feature = "inject");
    let mut run = AgentRun::new("first question").max_turns(4);
    let end = d.drive(&mut run, &Kill::default()).await;
    p("2 end", format!("{end:?}"));
    for t in &d.trace { p("2 trace", t); }
    p("2 requests the model saw (U=user A=assistant), per attempt", format!("{:?}", req_summary(&probe)));
    let last_prompts: Vec<String> = probe.requests().iter().map(|r| format!("{:?}", r.chat_history.last().map(|m| format!("{m:?}").contains("INJECTED")))).collect();
    p("2 last message of each request contains the injected text", format!("{last_prompts:?}"));
    // SAVED run: does it have the injected message?
    let saved = serde_json::to_string(&run).unwrap();
    p("2 saved run (final) contains injected text", saved.contains("INJECTED"));
    p("2 run.full_history() contains injected text", serde_json::to_string(&run.full_history()).unwrap().contains("INJECTED"));
    // Resume from the CallModel#2 snapshot: the model for turn 2 sees what?
    let snap2 = d.snapshots.iter().find(|(l, _)| l == "CallModel#2").map(|(_, j)| j.clone());
    if let Some(j) = snap2 {
        p("2 snapshot at CallModel#2 contains injected text", j.contains("INJECTED"));
        let mut run2: AgentRun = serde_json::from_str(&j).unwrap();
        let m2 = MockCompletionModel::from_stream_turns([vec![E::text("done"), E::final_response_with_total_tokens(1)]]);
        let pm2 = m2.clone();
        let mut host2 = SideEffects::new();
        let mut d2 = Driver::new(m2, spec(4), &mut host2);
        let end = d2.drive(&mut run2, &Kill::default()).await;
        p("2 resumed end", format!("{end:?}"));
        p("2 resumed request (what the model sees on turn 2 after a restart)", format!("{:?}", req_summary(&pm2)));
    }
}

pub async fn item2b() {
    use rig_agent::run::AgentRunStep;
    use rig_core::conversation::pairing::validate;
    use rig_core::message::Message;
    let tag = if cfg!(feature = "inject") { "VENDORED" } else { "STOCK" };
    println!("
=== ITEM 2b: injection during a running tool batch -- {tag} ===");
    let model = MockCompletionModel::from_stream_turns([
        vec![
            E::tool_call("ca", "effect", serde_json::json!({"tag":"A"})),
            E::tool_call("cb", "effect", serde_json::json!({"tag":"B"})),
            E::final_response_with_total_tokens(2),
        ],
        vec![E::text("ok"), E::final_response_with_total_tokens(1)],
    ]);
    let mut host = SideEffects::new();
    let mut d = Driver::new(model, spec(4), &mut host);
    let mut run = AgentRun::new("do A and B").max_turns(4);
    let end = d.drive(&mut run, &Kill { after_answers: Some(1), ..Default::default() }).await;
    drop(d);
    p("2b state", format!("{end:?} (1 of 2 tools answered)"));
    let kinds = |v: &[std::sync::Arc<Message>]| -> Vec<String> {
        v.iter().map(|m| { let t = format!("{m:?}"); if t.starts_with("User") { "U".to_string() } else { "A".to_string() } }).collect()
    };
    p("2b committed messages (run.messages())", format!("{:?}", kinds(run.messages())));
    p("2b full_history() (adds closure for unanswered calls?)", format!("{:?}", kinds(&run.full_history().into_iter().map(std::sync::Arc::new).collect::<Vec<_>>())));
    // What a naive host would build by hand: committed messages + the injected user message.
    let inj = Message::user("INJECTED: stop, wrong account");
    let mut naive: Vec<Message> = run.messages().iter().map(|m| (**m).clone()).collect();
    naive.push(inj.clone());
    p("2b pairing check of committed + injected (calls A,B unanswered)", format!("{:?}", validate(&naive)));
    #[cfg(feature = "inject")]
    {
        let r = run.inject_message(inj.clone());
        p("2b vendored inject_message mid-batch", format!("{:?}", r.as_ref().err().map(|e| e.to_string())));
        match run.next_step().unwrap() {
            AgentRunStep::CallTools { calls } => p("2b run still drivable: unanswered calls", calls.len()),
            o => p("2b unexpected", format!("{o:?}").chars().take(50).collect::<String>()),
        }
    }
    #[cfg(not(feature = "inject"))]
    p("2b stock: no API to call; append_entry stores an Aside only", "(see 2c)");
}

/// Stock workaround: the host keeps the injection as an Aside in the run (saved with it) and splices on every turn.
pub async fn item2c() {
    use rig_core::conversation::{Aside, Author};
    use rig_core::message::Message;
    println!("
=== ITEM 2c: STOCK workaround, injection kept as an Aside and spliced per turn ===");
    let model = MockCompletionModel::from_stream_turns([
        vec![E::tool_call("c1", "effect", serde_json::json!({"tag":"X"})), E::final_response_with_total_tokens(1)],
        vec![E::text("done"), E::final_response_with_total_tokens(1)],
    ]);
    let probe = model.clone();
    let mut host = SideEffects::new();
    let mut d = Driver::new(model, spec(4), &mut host);
    // Hook: rebuild the request transcript from the committed messages plus every `host.inject` aside.
    d.splice = Some(Box::new(|run: &AgentRun| {
        let mut v: Vec<std::sync::Arc<Message>> = run.messages().to_vec();
        let mut asides: Vec<&Aside> = run.entries_of("host.inject").collect();
        asides.sort_by_key(|a| a.value["at"].as_u64().unwrap_or(0));
        for (n, a) in asides.iter().enumerate() {
            let at = (a.value["at"].as_u64().unwrap_or(0) as usize + n).min(v.len());
            v.insert(at, std::sync::Arc::new(Message::user(a.value["text"].as_str().unwrap_or(""))));
        }
        (!asides.is_empty()).then_some(v)
    }));
    let mut run = AgentRun::new("first question").max_turns(4);
    // The user types while turn 1 is pending: record at = committed length (1: just the prompt) -> goes after the prompt.
    run.append_entry(Aside::new("host.inject", Author::default(), serde_json::json!({"at": 1, "text": "INJECTED: also the budget"})));
    let end = d.drive(&mut run, &Kill::default()).await;
    p("2c end", format!("{end:?}"));
    p("2c requests seen", format!("{:?}", req_summary(&probe)));
    let j = serde_json::to_string(&run).unwrap();
    p("2c saved run carries the injection (as an aside)", j.contains("INJECTED"));
    p("2c but run.messages() / full_history() do not", !serde_json::to_string(&run.full_history()).unwrap().contains("INJECTED"));
    // restart at CallModel#2: the aside survives, the splice re-applies
    let snap = d.snapshots.iter().find(|(l, _)| l == "CallModel#2").unwrap().1.clone();
    drop(d);
    let mut run2: AgentRun = serde_json::from_str(&snap).unwrap();
    let m2 = MockCompletionModel::from_stream_turns([vec![E::text("done"), E::final_response_with_total_tokens(1)]]);
    let pm2 = m2.clone();
    let mut host2 = SideEffects::new();
    let mut d2 = Driver::new(m2, spec(4), &mut host2);
    d2.splice = Some(Box::new(|run: &AgentRun| {
        let mut v: Vec<std::sync::Arc<Message>> = run.messages().to_vec();
        let asides: Vec<&Aside> = run.entries_of("host.inject").collect();
        for (n, a) in asides.iter().enumerate() {
            let at = (a.value["at"].as_u64().unwrap_or(0) as usize + n).min(v.len());
            v.insert(at, std::sync::Arc::new(Message::user(a.value["text"].as_str().unwrap_or(""))));
        }
        (!asides.is_empty()).then_some(v)
    }));
    let end = d2.drive(&mut run2, &Kill::default()).await;
    p("2c resumed end", format!("{end:?}"));
    p("2c resumed turn-2 request", format!("{:?}", req_summary(&pm2)));
}

async fn item3() {
    println!("\n=== ITEM 3: several tool calls per turn, crash between tools ===");
    let mut host = SideEffects::new();
    let mut d = Driver::new(model_three(), spec(3), &mut host);
    let mut run = AgentRun::new("do three things").max_turns(3);
    let end = d.drive(&mut run, &Kill { after_answers: Some(2), ..Default::default() }).await;
    drop(d);
    p("3 end", format!("{end:?}"));
    p("3 effects executed before crash", format!("{:?}", host.log));
    let saved = serde_json::to_string(&run).unwrap();
    p("3 committed messages at crash (prompt + assistant turn; the batch results are not committed yet)", run.messages().len());
    drop(run);
    let mut run: AgentRun = serde_json::from_str(&saved).unwrap();
    match run.next_step().unwrap() {
        rig_agent::run::AgentRunStep::CallTools { calls } => {
            let ids: Vec<String> = calls
                .iter()
                .map(|c| match c {
                    rig_agent::run::PendingToolCall::Execute(e) => e.id().to_string(),
                    _ => "?".into(),
                })
                .collect();
            p("3 reloaded run asks to execute", format!("{ids:?}"));
        }
        other => p("3 unexpected step", short(&other)),
    }
    let mut host2 = SideEffects::new();
    let m = MockCompletionModel::from_stream_turns([vec![E::text("all done"), E::final_response_with_total_tokens(1)]]);
    let mut d2 = Driver::new(m, spec(3), &mut host2);
    let end = d2.drive(&mut run, &Kill::default()).await;
    drop(d2);
    p("3 resumed end", format!("{end:?}"));
    p("3 effects executed after resume", format!("{:?}", host2.log));
    let h = run.full_history();
    p("3 final history msgs", h.len());
    let tr = serde_json::to_string(&h).unwrap();
    p("3 result of answered tool A kept (no re-exec)", tr.contains("ran:A") && tr.contains("ran:B") && tr.contains("ran:C"));
}

async fn item4() {
    println!("\n=== ITEM 4: idempotency journal in a ToolHost ===");
    let charge_model = || {
        MockCompletionModel::from_stream_turns([
            vec![E::tool_call("c1", "effect", serde_json::json!({"tag":"CHARGE"})), E::final_response_with_total_tokens(1)],
            vec![E::text("charged"), E::final_response_with_total_tokens(1)],
        ])
    };
    let tail_model = || MockCompletionModel::from_stream_turns([vec![E::text("charged"), E::final_response_with_total_tokens(1)]]);
    // Case A1: plain host. Crash AFTER the reply was saved and the tool ran, before the result was fed back.
    let mut host = SideEffects::new();
    let mut d = Driver::new(charge_model(), spec(3), &mut host);
    let mut run = AgentRun::new("charge the card").max_turns(3);
    let end = d.drive(&mut run, &Kill { after_exec_before_answer: Some(1), ..Default::default() }).await;
    drop(d);
    p("4A1 first life", format!("{end:?}"));
    let saved = serde_json::to_string(&run).unwrap();
    let mut run1: AgentRun = serde_json::from_str(&saved).unwrap();
    let mut plain = SideEffects::new();
    plain.log = host.log.clone(); // the world remembers the first execution
    let mut d1 = Driver::new(tail_model(), spec(3), &mut plain);
    let end = d1.drive(&mut run1, &Kill::default()).await;
    drop(d1);
    p("4A1 plain host resume end", format!("{end:?}"));
    p("4A1 world log (CHARGE twice = double charge)", format!("{:?}", plain.log));

    // Case A2: journal keyed by tool-call id (durable: here a JSON string).
    let mut journal = Journal::new(SideEffects::new());
    let mut run0 = AgentRun::new("charge the card").max_turns(3);
    let mut d = Driver::new(charge_model(), spec(3), &mut journal);
    let end = d.drive(&mut run0, &Kill { after_exec_before_answer: Some(1), ..Default::default() }).await;
    drop(d);
    p("4A2 first life", format!("{end:?}"));
    let run_saved = serde_json::to_string(&run0).unwrap();
    let journal_saved = journal.save();
    let mut run2: AgentRun = serde_json::from_str(&run_saved).unwrap();
    let mut journal2 = Journal::load(&journal_saved, SideEffects::new());
    journal2.inner.log = journal.inner.log.clone();
    let mut d2 = Driver::new(tail_model(), spec(3), &mut journal2);
    let end = d2.drive(&mut run2, &Kill::default()).await;
    drop(d2);
    p("4A2 journal resume end", format!("{end:?}"));
    p("4A2 world log (CHARGE once)", format!("{:?}", journal2.inner.log));
    p("4A2 journal hits", journal2.hits);

    // Case B: crash BEFORE the reply is saved. The durable state is CallModel; the model is asked again
    // and may choose differently (different call id).
    let mut journal = Journal::new(SideEffects::new());
    let model = MockCompletionModel::from_stream_turns([vec![
        E::tool_call("cX", "effect", serde_json::json!({"tag":"CHARGE-100"})),
        E::final_response_with_total_tokens(1),
    ]]);
    let mut run0 = AgentRun::new("charge the card").max_turns(3);
    let mut d = Driver::new(model, spec(3), &mut journal);
    let _ = d.drive(&mut run0, &Kill { after_exec_before_answer: Some(1), ..Default::default() }).await;
    let stale = d.snapshots[0].1.clone(); // last durable state: CallModel#1 (reply never saved)
    p("4B durable snapshot label", &d.snapshots[0].0);
    drop(d);
    let journal_saved = journal.save();
    let mut run2: AgentRun = serde_json::from_str(&stale).unwrap();
    let mut journal2 = Journal::load(&journal_saved, SideEffects::new());
    journal2.inner.log = journal.inner.log.clone();
    let m2 = MockCompletionModel::from_stream_turns([
        vec![E::tool_call("cY", "effect", serde_json::json!({"tag":"CHARGE-100"})), E::final_response_with_total_tokens(1)],
        vec![E::text("charged"), E::final_response_with_total_tokens(1)],
    ]);
    let mut d2 = Driver::new(m2, spec(3), &mut journal2);
    let end = d2.drive(&mut run2, &Kill::default()).await;
    drop(d2);
    p("4B resume end", format!("{end:?}"));
    p("4B world log (id-keyed journal cannot dedupe: new id cY)", format!("{:?}", journal2.inner.log));
    p("4B journal hits", journal2.hits);
}

#[tokio::main]
async fn main() {
    let which = std::env::args().nth(1).unwrap_or_else(|| "all".into());
    if which == "all" || which == "1" {
        item1().await;
    }
    if which == "all" || which == "2" {
        item2().await;
    }
    if which == "all" || which == "2" {
        item2b().await;
        item2c().await;
    }
    if which == "all" || which == "5" {
        hooks::item5().await;
        hooks::item5_resume().await;
    }
    if which == "all" || which == "6" {
        hooks::item6().await;
    }
    if which == "all" || which == "3" {
        item3().await;
    }
    if which == "all" || which == "4" {
        item4().await;
    }
    let _ = End::Done(String::new());
}
