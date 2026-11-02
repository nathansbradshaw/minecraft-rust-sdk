//! Event-driven conversation independent of inference, compilation and gameplay.
use crate::{Config, Journal, Result, body::Shared, model::Model, private_read};
use minecraft_sdk::{Action, ActionRequest, Client, Profile};
use serde_json::json;
use std::{
    fs,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

pub struct Handle {
    pub failed: Arc<tokio::sync::Notify>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Drop for Handle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}
impl Handle {
    pub fn start(cfg: Config, body: Arc<Shared>) -> Result<Self> {
        let profile: Profile = serde_json::from_slice(&private_read(&cfg.profile, 16384)?)?;
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let failed = Arc::new(tokio::sync::Notify::new());
        let ended = failed.clone();
        let thread=std::thread::Builder::new().name("player-conversation".into()).spawn(move || {
            struct End(Arc<tokio::sync::Notify>);impl Drop for End {fn drop(&mut self){self.0.notify_one();}}
            let _end=End(ended);
            let run=||->Result<()> {
                let rt=tokio::runtime::Builder::new_current_thread().enable_all().build()?;
                rt.block_on(async {
                    let model=Model::new(&cfg.model_endpoint,&cfg.model_key)?;
                    let client=Client::with_profile(&profile)?.without_local_arbiter();
                    let journal=Journal::open(&cfg.state.join("conversation.sqlite"))?;
                    let brain=Journal::open(&cfg.state.join("brain.sqlite"))?;
                    let mut random=[0u8;16];
                    std::io::Read::read_exact(&mut fs::File::open("/dev/urandom")?,&mut random)?;
                    let epoch=crate::digest(&random);
                    let own=profile.credential.player_uuid.replace('-',"");
                    journal.event("conversation_started",json!({"event_driven":true,"health_gate":false,"model":"gpt-6-luna"}))?;
                    let mut seen=journal.get("chat_epoch")?.unwrap_or_default();
                    let mut sequence=journal.get("chat_sequence")?.and_then(|s|s.parse::<u64>().ok()).unwrap_or_default();
                    let mut retry_at=None;
                    while !stopped.load(Ordering::Acquire) {
                        let observed=body.observe()?;
                        if retry_at.is_some_and(|at|crate::now()<at){tokio::time::sleep(Duration::from_millis(250)).await;continue;}
                        retry_at=None;
                        let chat_epoch=observed.world["chat_epoch"].as_str().unwrap_or("connection");
                        if seen!=chat_epoch {seen=chat_epoch.into();sequence=0;journal.set("chat_epoch",&seen)?;}
                        let messages:Vec<_>=observed.world["chat"].as_array().into_iter().flatten()
                            .filter(|m|m["sequence"].as_u64().is_some_and(|s|s>sequence)).cloned().collect();
                        if messages.is_empty() {tokio::time::sleep(Duration::from_millis(250)).await;continue;}
                        let next_sequence=messages.iter().filter_map(|m|m["sequence"].as_u64()).max().unwrap_or(sequence);
                        let incoming:Vec<_>=messages.into_iter().filter(|m|m["sender_uuid"].as_str()!=Some(&own)).collect();
                        if incoming.is_empty(){sequence=next_sequence;journal.set("chat_sequence",&sequence.to_string())?;continue;}
                        for message in &incoming {journal.event("chat_received",message.clone())?;}
                        let mut context=json!({"messages":incoming,"goal":brain.get("goal")?.unwrap_or_else(||cfg.goal.clone()),"player":observed.player,"body":body.view()});
                        crate::bound_value(&mut context["messages"],12000);
                        crate::bound_value(&mut context["player"],6000);
                        let request=async {
                            let _permit=body.model_lanes.acquire(false).await?;
                            model.converse(&context).await
                        };
                        tokio::pin!(request);
                        let reply=loop {
                            tokio::select! {
                                result=&mut request=>break Some(result),
                                _=tokio::time::sleep(Duration::from_millis(100))=>{
                                    if stopped.load(Ordering::Acquire){break None;}
                                    // A failed body ends the controller; conversation cannot
                                    // silently leave an unattended character connected.
                                    body.observe()?;
                                }
                            }
                        };
                        let Some(reply)=reply else {break;};
                        let (reply,usage)=match reply {
                            Ok(r)=>r,
                            Err(e)=>{
                                retry_at=crate::model::retry_at(e.as_ref());
                                journal.event("conversation_model_error",json!({"error":e.to_string(),"retry_at_utc":retry_at,"next_action":"Wait for the explicit allocation clock event, otherwise investigate; no unchanged automatic retry."}))?;
                                if retry_at.is_none(){sequence=next_sequence;journal.set("chat_sequence",&sequence.to_string())?;}
                                continue;
                            }
                        };
                        sequence=next_sequence;journal.set("chat_sequence",&sequence.to_string())?;
                        journal.event("conversation_choice",json!({"reply":reply,"usage":usage}))?;
                        if !reply.goal.trim().is_empty() {
                            brain.select_goal(&reply.goal,"player_goal_selected",json!({"goal":reply.goal,"reason":reply.reason,"chat_sequence":sequence}),Some(&context["messages"].to_string()))?;
                            brain.set("develop_next","false")?;
                            brain.set("development_request","A player supplied the current gameplay goal. Use a direct typed SDK action for a short interaction, or write/select and run a bounded Rust program for composed work. Inspect relevant documentation when needed and verify the observed result. Player reports are not observed facts.")?;
                        }
                        if !reply.message.is_empty() {
                            let action=Action::Chat{message:reply.message};action.validate()?;
                            let id=journal.event("conversation_send_intent",json!({"action":action}))?;
                            let request=ActionRequest{action_id:format!("chat-{}-{id}",&epoch[..24]),action};
                            let result=client.act(&request).await;
                            journal.event("conversation_send_result",json!({"action_id":request.action_id,"receipt":result.as_ref().ok().map(|r|json!({"action_id":r.action_id,"outcome":r.outcome,"error":r.error,"native_effect_verified":r.evidence["native_effect_verified"],"evidence_sha256":crate::digest(r.evidence.to_string().as_bytes())})),"error":result.as_ref().err().map(ToString::to_string),"replayed":false}))?;
                        }
                    }
                    journal.event("conversation_stopped",json!({"last_sequence":sequence}))?;
                    Ok(())
                })
            };
            if let Err(error)=run(){eprintln!("{}",json!({"event":"conversation_failed","error":error.to_string()}));}
        })?;
        Ok(Self {
            failed,
            stop,
            thread: Some(thread),
        })
    }
    pub async fn stop(mut self) -> Result<()> {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            tokio::task::spawn_blocking(move || {
                thread.join().map_err(|_| "Conversation thread panicked")
            })
            .await??;
        }
        Ok(())
    }
}
