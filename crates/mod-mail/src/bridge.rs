//! Glue between the UI (asker, secret store, events) and alouette-mail's traits.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use alouette_core::AppEvent;
use alouette_core::event::{EventSender, ModulePayload};
use alouette_store::secrets::SecretStore;
use alouette_ui::ask::{Asker, Question};

use crate::MODULE;
use crate::window::{Kind, Msg};

pub struct Secrets(pub Arc<dyn SecretStore>);

impl alouette_mail::alouette_store_like::Secrets for Secrets {
    fn get(&self, key: &str) -> Option<String> {
        self.0.get(key).ok().flatten()
    }
    fn set(&self, key: &str, value: &str) {
        let _ = self.0.set(key, value);
    }
    fn delete(&self, key: &str) {
        let _ = self.0.delete(key);
    }
}

pub struct Prompt {
    pub asker: Asker,
    pub events: EventSender,
    pub win: u64,
}

impl alouette_mail::Prompt for Prompt {
    fn ask(
        &self,
        title: &str,
        body: Vec<String>,
        prompt: &str,
        secret: bool,
    ) -> Pin<Box<dyn Future<Output = Option<String>> + Send + '_>> {
        Box::pin(self.asker.ask(Question { title: title.into(), body, prompt: prompt.into(), secret, yes_no: false }))
    }
    fn note(&self, text: &str) {
        send(&self.events, self.win, Kind::Note(text.to_string()));
    }
}

pub fn send(events: &EventSender, win: u64, kind: Kind) {
    let _ = events.send(AppEvent::Module { module: MODULE, payload: ModulePayload::new(Msg { win, kind }) });
}
