//! Spike 3 driver (scripted provider). Shared pieces live in common.rs.
include!("common.rs");

#[tokio::main]
async fn main() -> Result<()> {
    // CONTROL: uninterrupted run.
    let ctl_store = Arc::new(Store::default());
    seed(&ctl_store);
    let ctl = rig(ctl_store.clone(), vec![tool_turn(), text_turn(&["3"])]);
    let (em, _rx) = ctl.emitter();
    ctl.machine().run(&*ctl.store, "s", &em).await?;
    let control = ctl_store.json.lock().unwrap().clone();
    println!(
        "CONTROL  shape={:?} final={:?} model_calls={} tool_calls={}",
        shape(&control),
        final_text(&control),
        ctl.provider.calls.load(Ordering::SeqCst),
        ctl.tool_calls.load(Ordering::SeqCst)
    );

    // P1a: persisted BEFORE the model call (conversation = user msg only). Fresh machine and provider.
    let s = Arc::new(Store::default());
    seed(&s);
    let before = s.json.lock().unwrap().clone();
    let r = rig(s.clone(), vec![tool_turn(), text_turn(&["3"])]);
    let (em, _rx) = r.emitter();
    r.machine().run(&*r.store, "s", &em).await?;
    println!(
        "P1a  persisted-before-call ({} bytes) -> finished on fresh machine, shape==control: {}",
        before.len(),
        shape(&s.json.lock().unwrap()) == shape(&control)
    );

    // P1b: crash after the model reply, before it is persisted. Inference must re-run.
    let s = Arc::new(Store::default());
    seed(&s);
    *s.fail_apply_n.lock().unwrap() = Some(1);
    let r1 = rig(s.clone(), vec![tool_turn()]);
    let (em, _rx) = r1.emitter();
    let err = r1.machine().run(&*r1.store, "s", &em).await.err().map(|e| e.to_string());
    println!(
        "P1b  crash run: err={err:?} model_calls={} store_shape={:?}",
        r1.provider.calls.load(Ordering::SeqCst),
        shape(&s.json.lock().unwrap())
    );
    let r2 = rig(s.clone(), vec![tool_turn(), text_turn(&["3"])]);
    let (em, _rx) = r2.emitter();
    r2.machine().run(&*r2.store, "s", &em).await?;
    println!(
        "P1b  resumed on fresh machine: model_calls={} shape==control: {}",
        r2.provider.calls.load(Ordering::SeqCst),
        shape(&s.json.lock().unwrap()) == shape(&control)
    );

    // P1c: crash after the tool ran but before its response is persisted (2nd apply). Does the tool re-run?
    let s = Arc::new(Store::default());
    seed(&s);
    *s.fail_apply_n.lock().unwrap() = Some(2);
    let r1 = rig(s.clone(), vec![tool_turn(), text_turn(&["3"])]);
    let (em, _rx) = r1.emitter();
    let err = r1.machine().run(&*r1.store, "s", &em).await.err().map(|e| e.to_string());
    println!(
        "P1c  crash run: err={err:?} tool_calls={} store_shape={:?}",
        r1.tool_calls.load(Ordering::SeqCst),
        shape(&s.json.lock().unwrap())
    );
    let r2 = rig(s.clone(), vec![text_turn(&["3"])]);
    let (em, _rx) = r2.emitter();
    r2.machine().run(&*r2.store, "s", &em).await?;
    println!(
        "P1c  resumed: tool_calls_after_resume={} model_calls={} final={:?}",
        r2.tool_calls.load(Ordering::SeqCst),
        r2.provider.calls.load(Ordering::SeqCst),
        final_text(&s.json.lock().unwrap())
    );

    // P2a: streaming event order and what is persisted for a 3-chunk text turn.
    let s = Arc::new(Store::default());
    seed(&s);
    let r = rig(s.clone(), vec![tool_turn(), text_turn(&["The ", "answer ", "is 3"])]);
    let (em, mut rx) = r.emitter();
    r.machine().run(&*r.store, "s", &em).await?;
    drop(em);
    let mut ev = vec![];
    while let Some(e) = rx.recv().await {
        ev.push(match e {
            goose_agent::events::AgentEvent::Message(m) => format!(
                "Message({})",
                m.content
                    .iter()
                    .map(|b| match b {
                        MessageContent::Text(t) => format!("text:{:?}", t.text),
                        MessageContent::ToolRequest(_) => "tool_req".into(),
                        MessageContent::ToolResponse(_) => "tool_resp".into(),
                        _ => "other".into(),
                    })
                    .collect::<Vec<_>>()
                    .join(",")
            ),
            other => format!("{:?}", std::mem::discriminant(&other)),
        });
    }
    println!("P2a  {} events: {:?}", ev.len(), ev);
    println!("P2a  persisted: {:?}", shape(&s.json.lock().unwrap()));

    // P2b: cancel mid-stream after 1 chunk, then resume.
    let s = Arc::new(Store::default());
    seed(&s);
    let mut script = text_turn(&["The "]);
    script.push(It::CancelHang);
    let r = rig(s.clone(), vec![tool_turn(), script]);
    let (em, _rx) = r.emitter();
    let res = r.machine().run(&*r.store, "s", &em).await;
    println!(
        "P2b  cancel mid-stream: run_ok={} store_shape={:?}",
        res.is_ok(),
        shape(&s.json.lock().unwrap())
    );
    let r2 = rig(s.clone(), vec![text_turn(&["unused"])]);
    let (em, _rx) = r2.emitter();
    r2.machine().run(&*r2.store, "s", &em).await?;
    println!(
        "P2b  resume, no new user message: model_calls={}",
        r2.provider.calls.load(Ordering::SeqCst)
    );

    // P2c: transport cut mid-stream, then resume.
    let s = Arc::new(Store::default());
    seed(&s);
    let mut script = text_turn(&["The "]);
    script.push(It::Err(ProviderError::NetworkError("connection reset".into())));
    let r = rig(s.clone(), vec![tool_turn(), script]);
    let (em, _rx) = r.emitter();
    let res = r.machine().run(&*r.store, "s", &em).await;
    println!(
        "P2c  transport cut: run_ok={} store_shape={:?}",
        res.is_ok(),
        shape(&s.json.lock().unwrap())
    );
    let r2 = rig(s.clone(), vec![text_turn(&["3"])]);
    let (em, _rx) = r2.emitter();
    r2.machine().run(&*r2.store, "s", &em).await?;
    println!(
        "P2c  resume unchanged: model_calls={}",
        r2.provider.calls.load(Ordering::SeqCst)
    );
    Ok(())
}
