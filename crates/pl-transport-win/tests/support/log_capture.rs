// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! One tracing subscriber that captures a chosen target's events.
//!
//! Previously duplicated per test binary — once for `journal_bridge` and once
//! for `pl_transport`. The target is a parameter so both callers share one
//! implementation.

#![allow(dead_code)] // Each integration-test binary compiles this shared helper independently.

use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct CapturingSubscriber {
    target: &'static str,
    lines: Arc<Mutex<Vec<String>>>,
}

impl CapturingSubscriber {
    pub fn for_target(target: &'static str) -> Self {
        Self {
            target,
            lines: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// The shared buffer this subscriber appends to.
    pub fn lines(&self) -> Arc<Mutex<Vec<String>>> {
        self.lines.clone()
    }

    /// Install as the process-wide default. Only one call per test binary can
    /// succeed, so a binary that needs captured events keeps that to one test.
    pub fn install(&self) {
        tracing::dispatcher::set_global_default(tracing::Dispatch::new(self.clone()))
            .expect("install the capturing subscriber exactly once per test binary");
    }

    pub fn take(&self) -> Vec<String> {
        std::mem::take(&mut *self.lines.lock().unwrap())
    }

    pub fn joined(&self) -> String {
        self.lines.lock().unwrap().join("\n")
    }
}

impl tracing::Subscriber for CapturingSubscriber {
    fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
        metadata.target() == self.target
    }

    fn new_span(&self, _span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }

    fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}

    fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}

    fn event(&self, event: &tracing::Event<'_>) {
        if !self.enabled(event.metadata()) {
            return;
        }
        let mut visitor = LogVisitor::default();
        event.record(&mut visitor);
        self.lines.lock().unwrap().push(visitor.line);
    }

    fn enter(&self, _span: &tracing::span::Id) {}

    fn exit(&self, _span: &tracing::span::Id) {}
}

#[derive(Default)]
pub struct LogVisitor {
    pub line: String,
}

impl LogVisitor {
    fn field(&mut self, name: &str, value: impl std::fmt::Display) {
        if !self.line.is_empty() {
            self.line.push(' ');
        }
        self.line.push_str(name);
        self.line.push('=');
        self.line.push_str(&value.to_string());
    }
}

impl tracing::field::Visit for LogVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.field(field.name(), format!("{value:?}"));
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.field(field.name(), value);
    }

    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        self.field(field.name(), value);
    }

    fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
        self.field(field.name(), value);
    }

    fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
        self.field(field.name(), value);
    }
}
