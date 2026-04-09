use std::sync::{Arc, Mutex};

use uuid::Uuid;

use crate::types::claude::{Message, MessagesRequest};

#[derive(Clone)]
pub struct SessionStore {
    inner: Arc<Mutex<Vec<SessionEntry>>>,
}

#[derive(Debug, Clone)]
pub struct SessionPlan {
    pub session_key: String,
    pub previous_response_id: Option<String>,
    pub input_messages: Vec<Message>,
}

#[derive(Clone)]
struct SessionEntry {
    session_key: String,
    upstream_model: String,
    messages: Vec<Message>,
    response_id: String,
}

impl SessionStore {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn plan(&self, request: &MessagesRequest, upstream_model: &str) -> SessionPlan {
        let entries = self.lock_entries();
        match find_best_entry(&entries, request, upstream_model) {
            Some(entry) => SessionPlan::reuse(entry, request),
            None => SessionPlan::fresh(request.messages.clone()),
        }
    }

    pub fn commit(
        &self,
        plan: &SessionPlan,
        request: &MessagesRequest,
        upstream_model: &str,
        response_id: &str,
    ) {
        let mut entries = self.lock_entries();
        let entry = SessionEntry {
            session_key: plan.session_key.clone(),
            upstream_model: upstream_model.to_string(),
            messages: request.messages.clone(),
            response_id: response_id.to_string(),
        };
        if let Some(index) = entries
            .iter()
            .position(|item| item.session_key == entry.session_key)
        {
            entries[index] = entry;
            return;
        }
        entries.push(entry);
    }

    fn lock_entries(&self) -> std::sync::MutexGuard<'_, Vec<SessionEntry>> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl SessionPlan {
    fn fresh(messages: Vec<Message>) -> Self {
        Self {
            session_key: generate_session_key(),
            previous_response_id: None,
            input_messages: messages,
        }
    }

    fn reuse(entry: &SessionEntry, request: &MessagesRequest) -> Self {
        let prefix_len = entry.messages.len();
        Self {
            session_key: entry.session_key.clone(),
            previous_response_id: Some(entry.response_id.clone()),
            input_messages: request.messages[prefix_len..].to_vec(),
        }
    }
}

fn find_best_entry<'a>(
    entries: &'a [SessionEntry],
    request: &MessagesRequest,
    upstream_model: &str,
) -> Option<&'a SessionEntry> {
    entries
        .iter()
        .filter(|entry| is_prefix_match(entry, request, upstream_model))
        .max_by_key(|entry| entry.messages.len())
}

fn is_prefix_match(entry: &SessionEntry, request: &MessagesRequest, upstream_model: &str) -> bool {
    entry.upstream_model == upstream_model
        && request.messages.len() > entry.messages.len()
        && request.messages.starts_with(&entry.messages)
}

fn generate_session_key() -> String {
    format!("ccproxy_{}", Uuid::new_v4().simple())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::claude::{Message, MessageContent, MessagesRequest};

    fn request(messages: &[&str]) -> MessagesRequest {
        MessagesRequest {
            model: "claude-3-5-sonnet-20241022".into(),
            max_tokens: 1024,
            messages: messages
                .iter()
                .map(|text| Message {
                    role: "user".into(),
                    content: MessageContent::Text((*text).into()),
                })
                .collect(),
            system: None,
            stop_sequences: None,
            stream: Some(false),
            temperature: Some(1.0),
            top_p: None,
            top_k: None,
            metadata: None,
            tools: None,
            tool_choice: None,
            thinking: None,
        }
    }

    #[test]
    fn creates_fresh_plan_without_prior_session() {
        let store = SessionStore::new();
        let plan = store.plan(&request(&["hello"]), "gpt-5.4");
        assert!(plan.previous_response_id.is_none());
        assert_eq!(plan.input_messages.len(), 1);
    }

    #[test]
    fn reuses_longest_matching_session_prefix() {
        let store = SessionStore::new();
        let first = request(&["hello"]);
        let second = request(&["hello", "follow up"]);
        store.commit(
            &SessionPlan::fresh(first.messages.clone()),
            &first,
            "gpt-5.4",
            "resp_1",
        );
        store.commit(
            &SessionPlan {
                session_key: "ccproxy_fixed".into(),
                previous_response_id: Some("resp_1".into()),
                input_messages: vec![second.messages[1].clone()],
            },
            &second,
            "gpt-5.4",
            "resp_2",
        );

        let extended = request(&["hello", "follow up", "new user turn"]);
        let plan = store.plan(&extended, "gpt-5.4");

        assert_eq!(plan.previous_response_id.as_deref(), Some("resp_2"));
        assert_eq!(plan.input_messages, vec![extended.messages[2].clone()]);
    }

    #[test]
    fn does_not_reuse_across_model_boundaries() {
        let store = SessionStore::new();
        let req = request(&["hello"]);
        let plan = SessionPlan::fresh(req.messages.clone());
        store.commit(&plan, &req, "gpt-5.4", "resp_1");

        let extended = request(&["hello", "world"]);
        let next_plan = store.plan(&extended, "gpt-5.4-mini");

        assert!(next_plan.previous_response_id.is_none());
        assert_eq!(next_plan.input_messages.len(), 2);
    }
}
