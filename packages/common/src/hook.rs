use anyhow::Result;
use async_trait::async_trait;
use std::{collections::HashMap, sync::Arc};

use crate::event::{Event, GenericEvent};

// `async_trait` marks each method `#[must_use]` on top of its boxed future.
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait Hook<E: Event>: Send + Sync {
    type Output: Event;
    type Context: Send + Sync + 'static;

    fn id(&self) -> &str;
    fn topics(&self) -> &[String];

    fn on_register(&self, _ctx: Self::Context) -> Result<()> {
        Ok(())
    }
    fn on_unregister(&self, _ctx: Self::Context) -> Result<()> {
        Ok(())
    }
    async fn on_event(&self, ctx: Self::Context, e: &E) -> Result<HookAction<E, Self::Output>>;
}

pub enum HookAction<E: Event, O: Event = E> {
    Pass,
    Stop,
    Modified(E),
    Reject(String),
    Chain(Vec<O>),
}

pub type GenericHookAction = HookAction<GenericEvent>;

// `async_trait` marks each method `#[must_use]` on top of its boxed future.
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait GenericHook: Send + Sync {
    type Context: Send + Sync + 'static;

    fn id(&self) -> &str;
    fn topics(&self) -> &[String];

    fn on_register(&self, _ctx: Self::Context) -> Result<()> {
        Ok(())
    }
    fn on_unregister(&self, _ctx: Self::Context) -> Result<()> {
        Ok(())
    }
    async fn on_event(&self, ctx: Self::Context, e: &GenericEvent) -> Result<GenericHookAction>;
}

pub struct HookAdapter<E: Event, H: Hook<E>> {
    hook: Arc<H>,
    _phantom: std::marker::PhantomData<E>,
}

#[async_trait]
impl<E: Event, H: Hook<E>> GenericHook for HookAdapter<E, H> {
    type Context = H::Context;

    fn id(&self) -> &str {
        self.hook.id()
    }
    fn topics(&self) -> &[String] {
        self.hook.topics()
    }
    async fn on_event(
        &self,
        ctx: H::Context,
        generic_event: &GenericEvent,
    ) -> Result<GenericHookAction> {
        let typed_event: E = E::from_generic_event(generic_event)?;
        let action = self.hook.on_event(ctx, &typed_event).await?;
        match action {
            HookAction::Pass => Ok(GenericHookAction::Pass),
            HookAction::Stop => Ok(GenericHookAction::Stop),
            HookAction::Modified(modified_event) => Ok(GenericHookAction::Modified(
                modified_event.to_generic_event(),
            )),
            HookAction::Reject(reason) => Ok(GenericHookAction::Reject(reason)),
            HookAction::Chain(events) => Ok(GenericHookAction::Chain(
                events.into_iter().map(|e| e.to_generic_event()).collect(),
            )),
        }
    }

    fn on_register(&self, ctx: H::Context) -> Result<()> {
        self.hook.on_register(ctx)
    }

    fn on_unregister(&self, ctx: H::Context) -> Result<()> {
        self.hook.on_unregister(ctx)
    }
}

#[derive(Clone)]
pub struct HookRegistry<Context: Send + Sync + Copy + 'static = ()> {
    ctx: Context,
    hooks: HashMap<String, Vec<Arc<dyn GenericHook<Context = Context>>>>,
}

impl<C: Send + Sync + Copy + 'static> HookRegistry<C> {
    pub fn new(ctx: C) -> Self {
        Self {
            ctx,
            hooks: HashMap::new(),
        }
    }

    pub fn add_hook<E: Event + 'static, H: Hook<E, Context = C> + 'static>(
        &mut self,
        hook: H,
    ) -> Result<()> {
        let adapter = Arc::new(HookAdapter::<E, H> {
            hook: Arc::new(hook),
            _phantom: std::marker::PhantomData,
        });
        adapter.on_register(self.ctx)?;

        for topic in adapter.topics() {
            self.hooks
                .entry(topic.clone())
                .or_default()
                .push(adapter.clone());
        }
        Ok(())
    }

    pub fn add_generic_hook(&mut self, hook: Arc<dyn GenericHook<Context = C>>) -> Result<()> {
        hook.on_register(self.ctx)?;
        for topic in hook.topics() {
            self.hooks
                .entry(topic.clone())
                .or_default()
                .push(hook.clone());
        }
        Ok(())
    }

    pub fn remove_hook(&mut self, hook_id: &str) -> Result<()> {
        let hooks = &mut self.hooks;

        for hooks_list in hooks.values_mut() {
            if let Some(pos) = hooks_list.iter().position(|h| h.id() == hook_id) {
                let hook = hooks_list.remove(pos);
                hook.on_unregister(self.ctx)?;
                return Ok(());
            }
        }

        Err(anyhow::anyhow!("Hook not found: {}", hook_id))
    }

    pub fn remove_hooks_by_id(&mut self, hook_id: &str) {
        for hooks_list in self.hooks.values_mut() {
            hooks_list.retain(|h| h.id() != hook_id);
        }
    }

    pub async fn trigger<E: Event>(&self, event: &E) -> Result<HookAction<E>> {
        let topic = event.topic();
        let hooks = match self.hooks.get(topic) {
            Some(h) if !h.is_empty() => h,
            _ => return Ok(HookAction::Pass),
        };

        let mut generic_event = event.to_generic_event();
        for hook in hooks {
            let action = hook.on_event(self.ctx, &generic_event).await?;

            match action {
                HookAction::Pass => {}
                HookAction::Modified(new_event) => {
                    generic_event = new_event;
                }
                HookAction::Stop => {
                    return Ok(HookAction::Stop);
                }
                HookAction::Reject(reason) => {
                    return Ok(HookAction::Reject(reason));
                }
                HookAction::Chain(events) => {
                    return Ok(HookAction::Chain(
                        events
                            .into_iter()
                            .map(|e| E::from_generic_event(&e))
                            .collect::<Result<Vec<E>>>()?,
                    ));
                }
            }
        }

        Ok(HookAction::Modified(E::from_generic_event(&generic_event)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FixedActionHook {
        id: String,
        topics: Vec<String>,
        action: GenericHookAction,
    }

    #[async_trait]
    impl GenericHook for FixedActionHook {
        type Context = ();

        fn id(&self) -> &str {
            &self.id
        }
        fn topics(&self) -> &[String] {
            &self.topics
        }
        async fn on_event(&self, _ctx: (), _e: &GenericEvent) -> Result<GenericHookAction> {
            Ok(match &self.action {
                HookAction::Pass => HookAction::Pass,
                HookAction::Stop => HookAction::Stop,
                HookAction::Reject(r) => HookAction::Reject(r.clone()),
                HookAction::Modified(e) => HookAction::Modified(e.clone()),
                HookAction::Chain(v) => HookAction::Chain(v.clone()),
            })
        }
    }

    fn make_event(topic: &str) -> GenericEvent {
        GenericEvent {
            topic: topic.to_string(),
            payload: serde_json::json!({}),
        }
    }

    #[tokio::test]
    async fn reject_action_is_returned_as_ok_not_err() {
        let mut registry = HookRegistry::new(());
        let hook = FixedActionHook {
            id: "rejector".into(),
            topics: vec!["test".into()],
            action: HookAction::Reject("cooldown active".into()),
        };
        registry.add_generic_hook(Arc::new(hook)).unwrap();

        let event = make_event("test");
        let result = registry.trigger(&event).await;

        assert!(result.is_ok(), "Reject should be Ok, not Err");
        match result.unwrap() {
            HookAction::Reject(reason) => assert_eq!(reason, "cooldown active"),
            other => panic!("Expected Reject, got {:?}", std::mem::discriminant(&other)),
        }
    }

    #[tokio::test]
    async fn stop_action_halts_hook_chain() {
        let mut registry = HookRegistry::new(());
        let hook = FixedActionHook {
            id: "stopper".into(),
            topics: vec!["test".into()],
            action: HookAction::Stop,
        };
        registry.add_generic_hook(Arc::new(hook)).unwrap();

        let event = make_event("test");
        let result = registry.trigger(&event).await.unwrap();
        assert!(matches!(result, HookAction::Stop));
    }

    #[tokio::test]
    async fn topic_with_no_hooks_returns_pass() {
        let registry = HookRegistry::new(());
        let event = make_event("no_hooks");
        let result = registry.trigger(&event).await.unwrap();
        assert!(matches!(result, HookAction::Pass));
    }
}
