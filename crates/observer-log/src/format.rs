// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Structured safe event formatter and layer builder.

use std::borrow::Cow;
use std::fmt;
use std::time::SystemTime;

use tracing::field::Visit;
use tracing::{Event, Subscriber};
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::{FmtContext, FormatEvent, FormatFields};
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::EnvFilter;

use crate::record::{
    capped_len_debug, capped_len_display, capped_len_str, encode, Field, FieldValue, Header,
    EXCLUDED_LEN_CAP,
};

/// Visitor that collects only safe field representations.
pub(crate) struct SafeVisitor {
    pub(crate) fields: Vec<Field>,
}

impl SafeVisitor {
    pub(crate) fn new() -> Self {
        Self { fields: Vec::new() }
    }
}

impl Visit for SafeVisitor {
    fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
        self.fields.push(Field {
            name: Cow::Borrowed(field.name()),
            value: FieldValue::I128(value as i128),
        });
    }

    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        self.fields.push(Field {
            name: Cow::Borrowed(field.name()),
            value: FieldValue::U128(value as u128),
        });
    }

    fn record_i128(&mut self, field: &tracing::field::Field, value: i128) {
        self.fields.push(Field {
            name: Cow::Borrowed(field.name()),
            value: FieldValue::I128(value),
        });
    }

    fn record_u128(&mut self, field: &tracing::field::Field, value: u128) {
        self.fields.push(Field {
            name: Cow::Borrowed(field.name()),
            value: FieldValue::U128(value),
        });
    }

    fn record_f64(&mut self, field: &tracing::field::Field, value: f64) {
        self.fields.push(Field {
            name: Cow::Borrowed(field.name()),
            value: FieldValue::F64(value),
        });
    }

    fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
        self.fields.push(Field {
            name: Cow::Borrowed(field.name()),
            value: FieldValue::Bool(value),
        });
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.fields.push(Field {
            name: Cow::Borrowed(field.name()),
            value: FieldValue::Len(capped_len_str(value)),
        });
    }

    fn record_bytes(&mut self, field: &tracing::field::Field, value: &[u8]) {
        self.fields.push(Field {
            name: Cow::Borrowed(field.name()),
            value: FieldValue::Len(value.len().min(EXCLUDED_LEN_CAP)),
        });
    }

    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn fmt::Debug) {
        self.fields.push(Field {
            name: Cow::Borrowed(field.name()),
            value: FieldValue::Len(capped_len_debug(value)),
        });
    }

    fn record_error(
        &mut self,
        field: &tracing::field::Field,
        value: &(dyn std::error::Error + 'static),
    ) {
        self.fields.push(Field {
            name: Cow::Borrowed(field.name()),
            value: FieldValue::Len(capped_len_display(value)),
        });
    }
}

/// Formatter for fields that renders safe name=type:value tokens.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct SafeFields;

impl<'writer> FormatFields<'writer> for SafeFields {
    fn format_fields<R: tracing_subscriber::field::RecordFields>(
        &self,
        mut writer: Writer<'writer>,
        fields: R,
    ) -> fmt::Result {
        let mut visitor = SafeVisitor::new();
        fields.record(&mut visitor);
        let mut first = true;
        for field in visitor.fields {
            if !first {
                write!(writer, " ")?;
            }
            first = false;
            match field.value {
                FieldValue::Bool(b) => write!(writer, "{}=b:{b}", field.name)?,
                FieldValue::I128(i) => write!(writer, "{}=i:{i}", field.name)?,
                FieldValue::U128(u) => write!(writer, "{}=u:{u}", field.name)?,
                FieldValue::F64(f) => {
                    if f.is_nan() {
                        write!(writer, "{}=f:nan", field.name)?;
                    } else if f == f64::INFINITY {
                        write!(writer, "{}=f:inf", field.name)?;
                    } else if f == f64::NEG_INFINITY {
                        write!(writer, "{}=f:-inf", field.name)?;
                    } else {
                        write!(writer, "{}=f:{f:?}", field.name)?;
                    }
                }
                FieldValue::Len(l) => write!(writer, "{}=l:{l}", field.name)?,
            }
        }
        Ok(())
    }
}

/// Structured safe event formatter.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct SafeFormat;

impl<S, N> FormatEvent<S, N> for SafeFormat
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'writer> FormatFields<'writer> + 'static,
{
    fn format_event(
        &self,
        ctx: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &Event<'_>,
    ) -> fmt::Result {
        let meta = event.metadata();
        let mut visitor = SafeVisitor::new();
        event.record(&mut visitor);

        let mut span_names = Vec::new();
        let mut all_fields = visitor.fields;
        if let Some(scope) = ctx.event_scope() {
            for span in scope.from_root() {
                span_names.push(span.name());
                let exts = span.extensions();
                if let Some(fields) = exts.get::<tracing_subscriber::fmt::FormattedFields<N>>() {
                    for token in fields.fields.split_whitespace() {
                        if let Some((name, type_val)) = token.split_once('=') {
                            if let Some((typ, val)) = type_val.split_once(':') {
                                let parsed_val = match typ {
                                    "b" => match val {
                                        "true" => Some(FieldValue::Bool(true)),
                                        "false" => Some(FieldValue::Bool(false)),
                                        _ => None,
                                    },
                                    "i" => val.parse::<i128>().ok().map(FieldValue::I128),
                                    "u" => val.parse::<u128>().ok().map(FieldValue::U128),
                                    "f" => val.parse::<f64>().ok().map(FieldValue::F64),
                                    "l" => val.parse::<usize>().ok().map(FieldValue::Len),
                                    _ => None,
                                };
                                if let Some(value) = parsed_val {
                                    all_fields.push(Field {
                                        name: Cow::Owned(name.to_string()),
                                        value,
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }

        let header = Header {
            now: SystemTime::now(),
            level: meta.level().as_str(),
            target: meta.target(),
            module: meta.module_path(),
            file: meta.file(),
            line: meta.line(),
            column: None,
            spans: &span_names,
        };

        let line = encode(&header, &all_fields);
        writer.write_str(&line)
    }
}

/// Build the subscriber builder with safe formatting and custom filter/writer.
pub(crate) fn layer_builder<W>(
    filter: EnvFilter,
    writer: W,
) -> tracing_subscriber::fmt::SubscriberBuilder<SafeFields, SafeFormat, EnvFilter, W>
where
    W: for<'writer> tracing_subscriber::fmt::MakeWriter<'writer> + 'static,
{
    tracing_subscriber::fmt()
        .with_ansi(false)
        .event_format(SafeFormat)
        .fmt_fields(SafeFields)
        .with_env_filter(filter)
        .with_writer(writer)
}

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, UNIX_EPOCH};

    use tracing_subscriber::fmt::MakeWriter;
    use tracing_subscriber::EnvFilter;

    use super::*;
    use crate::panic::panic_line;
    use crate::record::parse_line;

    #[derive(Clone, Default)]
    struct MemoryWriter(Arc<Mutex<Vec<u8>>>);

    impl<'a> MakeWriter<'a> for MemoryWriter {
        type Writer = MemoryWriterGuard;
        fn make_writer(&'a self) -> Self::Writer {
            MemoryWriterGuard(self.0.clone())
        }
    }

    struct MemoryWriterGuard(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for MemoryWriterGuard {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            let mut lock = self.0.lock().unwrap();
            lock.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    const SPAN_SENTINEL: &str = "SPAN_SECRET_TOKEN_99999";
    const CREDENTIAL_SENTINEL: &str = "VERY_SECRET_CREDENTIAL_12345";
    const PAIR_LINK_SENTINEL: &str = "pair://solstone.internal/token?secret=67890";
    const PATH_SENTINEL: &str = "/home/john_doe/.config/solstone/secret.dat";
    const TITLE_SENTINEL: &str = "Confidential Banking Document Window";
    const HOSTNAME_SENTINEL: &str = "prod-db-internal-01.internal";
    const MODEL_SENTINEL: &str = "Gemini-Ultra-Secret-Model-v9";

    fn emit_helper_event(secret: &str) {
        tracing::info!(
            target: "obs_test",
            secret = secret,
            "event message with secret {}",
            secret
        );
    }

    #[test]
    pub(crate) fn safe_records_drop_sentinels_and_keep_identity() {
        let buffer = Arc::new(Mutex::new(Vec::new()));
        let writer = MemoryWriter(buffer.clone());
        let subscriber = layer_builder(EnvFilter::new("trace"), writer).finish();

        let mut event_line = 0;
        tracing::subscriber::with_default(subscriber, || {
            let span = tracing::info_span!("join", session = %display(SPAN_SENTINEL));
            let _guard = span.enter();

            event_line = line!() + 1;
            tracing::info!(
                target: "obs_test",
                code = 7u64,
                delta = -3i64,
                flag = true,
                token = CREDENTIAL_SENTINEL,
                note = "paired",
                error = %display(PAIR_LINK_SENTINEL),
                "credential {} path {} title {} host {} model {}",
                CREDENTIAL_SENTINEL,
                PATH_SENTINEL,
                TITLE_SENTINEL,
                HOSTNAME_SENTINEL,
                MODEL_SENTINEL,
            );
        });

        let output = String::from_utf8(buffer.lock().unwrap().clone()).expect("valid utf8 output");
        assert!(output.ends_with('\n'), "output must be newline terminated");
        let trimmed_line = output.trim_end_matches('\n');

        assert!(
            parse_line(trimmed_line).is_some(),
            "line must parse according to grammar: {trimmed_line}"
        );

        assert!(output.contains("lvl=INFO"));
        assert!(output.contains("tgt=obs_test"));
        assert!(output.contains(&format!("mod={}", module_path!())));
        assert!(output.contains(&format!("loc={event_line}")));
        assert!(output.contains("spans=join"));
        assert!(output.contains("code=u:7"));
        assert!(output.contains("delta=i:-3"));
        assert!(output.contains("flag=b:true"));
        assert!(output.contains(&format!("token=l:{}", CREDENTIAL_SENTINEL.len())));
        assert!(output.contains("note=l:6"));
        assert!(output.contains(&format!("session=l:{}", SPAN_SENTINEL.len())));
        assert!(output.contains("message=l:"));

        assert!(!output.contains(SPAN_SENTINEL));
        assert!(!output.contains(CREDENTIAL_SENTINEL));
        assert!(!output.contains(PAIR_LINK_SENTINEL));
        assert!(!output.contains(PATH_SENTINEL));
        assert!(!output.contains(TITLE_SENTINEL));
        assert!(!output.contains(HOSTNAME_SENTINEL));
        assert!(!output.contains(MODEL_SENTINEL));
        assert!(!output.contains("paired"));

        let lines: Vec<&str> = output.lines().collect();
        assert_eq!(
            lines.len(),
            1,
            "FmtSpan must stay off; exactly one line should be emitted, got: {output:?}"
        );

        // Helper event byte equality for equal-length secrets
        let buf_a = Arc::new(Mutex::new(Vec::new()));
        let sub_a = layer_builder(EnvFilter::new("trace"), MemoryWriter(buf_a.clone())).finish();
        tracing::subscriber::with_default(sub_a, || {
            emit_helper_event("SECRET_AAAA_123");
        });
        let out_a = String::from_utf8(buf_a.lock().unwrap().clone()).unwrap();

        let buf_b = Arc::new(Mutex::new(Vec::new()));
        let sub_b = layer_builder(EnvFilter::new("trace"), MemoryWriter(buf_b.clone())).finish();
        tracing::subscriber::with_default(sub_b, || {
            emit_helper_event("SECRET_BBBB_456");
        });
        let out_b = String::from_utf8(buf_b.lock().unwrap().clone()).unwrap();

        fn mask_ts(s: &str) -> String {
            if let Some(start) = s.find("ts=") {
                if let Some(end) = s[start..].find(' ') {
                    let mut res = s[..start].to_string();
                    res.push_str("ts=TS");
                    res.push_str(&s[start + end..]);
                    return res;
                }
            }
            s.to_string()
        }
        assert_eq!(mask_ts(&out_a), mask_ts(&out_b));

        // Two panic_line calls
        let now = UNIX_EPOCH + Duration::from_millis(1_700_000_000_000);
        let p1 = panic_line(
            now,
            "thread_AAA",
            Some("/home/user/code/file1.rs"),
            Some(10),
            Some(5),
            "payload_AAA",
        );
        let p2 = panic_line(
            now,
            "thread_BBB",
            Some("C:\\Windows\\System32\\file2.rs"),
            Some(10),
            Some(5),
            "payload_BBB",
        );
        assert_eq!(p1, p2);
        assert!(p1.contains("file=-"));
        assert!(!p1.contains("/home/user"));
        assert!(!p1.contains("Windows"));
        assert!(!p1.contains("payload_AAA"));
        assert!(!p1.contains("thread_AAA"));
    }
}
