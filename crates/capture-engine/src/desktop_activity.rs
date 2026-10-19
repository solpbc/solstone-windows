// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Desktop activity scheduler: interval management, boundary split at rotation,
//! URL re-read cadence, generation validation, and availability tracking.

use std::collections::HashMap;

use observer_exclusion::{foreground_excluded, ExclusionRules};
use observer_model::desktop_activity::{
    normalize_url, ActivityState, AvailabilityCapability, AvailabilityStatus, DesktopActivityRow,
    DesktopActivitySource, DesktopActivityUrlRequest, DesktopActivityUrlResult,
};

/// Generates the payload filename for desktop activity: `{segment}_{artifact-id}_desktop_activity.jsonl`
pub fn activity_file_name(
    boundary_epoch_secs: u64,
    offset_secs: i64,
    artifact_id: &str,
    period_secs: u64,
) -> String {
    let seg =
        observer_pl::civil::segment_key_string_local(boundary_epoch_secs, offset_secs, period_secs);
    format!("{seg}_{artifact_id}_desktop_activity.jsonl")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenForeground {
    pub start_ms: u64,
    pub window_id: u64,
    pub app: String,
    pub exe_name: String,
    pub raw_title: String,
    pub title: Option<String>,
    pub url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenActivity {
    pub start_ms: u64,
    pub state: ActivityState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenAvailability {
    pub start_ms: u64,
    pub status: AvailabilityStatus,
}

#[derive(Debug, Clone, Default)]
pub struct UrlScheduleState {
    pub generation: u64,
    pub requested_window_id: Option<u64>,
    pub requested_at_ms: Option<u64>,
    pub last_poll_at_ms: Option<u64>,
    pub in_flight: bool,
}

/// Pure scheduler for desktop activity.
pub struct DesktopActivityScheduler {
    pub window_titles: bool,
    pub page_addresses: bool,
    pub exclusion_rules: ExclusionRules,
    pub artifact_id: String,
    pub open_foreground: Option<OpenForeground>,
    pub open_activity: Option<OpenActivity>,
    pub open_availabilities: HashMap<(AvailabilityCapability, String), OpenAvailability>,
    pub url_state: UrlScheduleState,
    pub is_locked: bool,
}

impl DesktopActivityScheduler {
    pub fn new(
        window_titles: bool,
        page_addresses: bool,
        exclusion_rules: ExclusionRules,
        artifact_id: String,
    ) -> Self {
        Self {
            window_titles,
            page_addresses,
            exclusion_rules,
            artifact_id,
            open_foreground: None,
            open_activity: None,
            open_availabilities: HashMap::new(),
            url_state: UrlScheduleState::default(),
            is_locked: false,
        }
    }

    pub fn set_preferences<S: DesktopActivitySource + ?Sized>(
        &mut self,
        source: &mut S,
        window_titles: bool,
        page_addresses: bool,
        now_ms: u64,
    ) -> Vec<DesktopActivityRow> {
        let mut closed = Vec::new();

        let titles_changed = self.window_titles != window_titles;
        let addresses_changed = self.page_addresses != page_addresses;

        let turning_off_titles = self.window_titles && !window_titles;
        let turning_off_addresses = self.page_addresses && !page_addresses;

        self.window_titles = window_titles;
        self.page_addresses = page_addresses;

        if turning_off_titles || turning_off_addresses {
            self.url_state.generation += 1;
            self.url_state.in_flight = false;
            self.url_state.requested_window_id = None;
            self.url_state.requested_at_ms = None;
            source.cancel_url();
        }

        if turning_off_addresses {
            if let Some(avail) = self.close_availability(now_ms, "uia", AvailabilityCapability::Url)
            {
                closed.push(avail);
            }
        }

        if let Some(mut fg) = self.open_foreground.take() {
            let mut needs_split = false;
            let new_title = if window_titles {
                Some(fg.raw_title.clone())
            } else {
                None
            };
            let new_url = if page_addresses { fg.url.clone() } else { None };

            if (titles_changed && fg.title != new_title) || (addresses_changed && fg.url != new_url)
            {
                needs_split = true;
            }

            if needs_split {
                closed.push(DesktopActivityRow::foreground(
                    fg.start_ms,
                    now_ms.max(fg.start_ms),
                    fg.app.clone(),
                    Some(fg.exe_name.clone()),
                    fg.title.clone(),
                    fg.url.clone(),
                ));
                fg.start_ms = now_ms;
                fg.title = new_title;
                fg.url = new_url;
            }
            self.open_foreground = Some(fg);
        }

        closed
    }

    pub fn set_exclusion_rules<S: DesktopActivitySource + ?Sized>(
        &mut self,
        source: &mut S,
        rules: ExclusionRules,
        now_ms: u64,
    ) -> Vec<DesktopActivityRow> {
        let mut closed = Vec::new();
        self.exclusion_rules = rules;

        if let Some(fg) = self.open_foreground.as_ref() {
            if foreground_excluded(&self.exclusion_rules, &fg.exe_name, &fg.raw_title) {
                let old = self.open_foreground.take().unwrap();
                closed.push(DesktopActivityRow::foreground(
                    old.start_ms,
                    now_ms.max(old.start_ms),
                    old.app,
                    Some(old.exe_name),
                    old.title,
                    old.url,
                ));
                self.url_state.generation += 1;
                self.url_state.in_flight = false;
                self.url_state.requested_window_id = None;
                self.url_state.requested_at_ms = None;
                source.cancel_url();
            }
        }
        closed
    }

    pub fn poll_and_step<S: DesktopActivitySource + ?Sized>(
        &mut self,
        source: &mut S,
        now_ms: u64,
    ) -> Vec<DesktopActivityRow> {
        let mut rows = Vec::new();

        // 1. Poll probe events
        let events = source.poll_events();
        for event in events {
            rows.extend(self.handle_event(source, event, now_ms));
        }

        // 2. Check URL request timeout (2s deadline)
        if self.url_state.in_flight {
            if let Some(req_time) = self.url_state.requested_at_ms {
                if now_ms.saturating_sub(req_time) >= 2000 {
                    source.cancel_url();

                    if let Some(avail_row) = self.transition_availability(
                        now_ms,
                        "uia",
                        AvailabilityCapability::Url,
                        AvailabilityStatus::Unavailable,
                    ) {
                        rows.push(avail_row);
                    }

                    if let Some(fg) = self.open_foreground.as_mut() {
                        if fg.url.is_some() {
                            let old = fg.clone();
                            rows.push(DesktopActivityRow::foreground(
                                old.start_ms,
                                now_ms.max(old.start_ms),
                                old.app,
                                Some(old.exe_name.clone()),
                                old.title,
                                old.url,
                            ));
                            fg.start_ms = now_ms;
                            fg.url = None;
                        }
                    }

                    self.url_state.generation += 1;
                    self.url_state.in_flight = false;
                    self.url_state.requested_at_ms = None;
                    self.url_state.requested_window_id = None;
                }
            }
        }

        // 3. Poll URL result from source
        if let Some(result) = source.poll_url_result() {
            rows.extend(self.handle_url_result(source, result, now_ms));
        }

        // 4. Cadence check for browser document URL (4s re-read)
        if let Some(fg) = self.open_foreground.as_ref() {
            if self.page_addresses && !self.is_locked {
                let is_browser = fg.exe_name == "msedge.exe" || fg.exe_name == "chrome.exe";
                if is_browser {
                    let should_request = if self.url_state.in_flight {
                        false
                    } else {
                        match self.url_state.last_poll_at_ms {
                            None => true,
                            Some(last) => now_ms.saturating_sub(last) >= 4000,
                        }
                    };

                    if should_request {
                        self.url_state.generation += 1;
                        self.url_state.in_flight = true;
                        self.url_state.requested_window_id = Some(fg.window_id);
                        self.url_state.requested_at_ms = Some(now_ms);
                        self.url_state.last_poll_at_ms = Some(now_ms);
                        source.request_url(DesktopActivityUrlRequest {
                            generation: self.url_state.generation,
                            window_id: fg.window_id,
                            exe_name: fg.exe_name.clone(),
                        });
                    }
                } else {
                    if let Some(avail_row) = self.transition_availability(
                        now_ms,
                        "uia",
                        AvailabilityCapability::Url,
                        AvailabilityStatus::Unavailable,
                    ) {
                        rows.push(avail_row);
                    }
                }
            }
        }

        rows
    }

    fn handle_url_result<S: DesktopActivitySource + ?Sized>(
        &mut self,
        source: &mut S,
        result: DesktopActivityUrlResult,
        now_ms: u64,
    ) -> Vec<DesktopActivityRow> {
        let mut rows = Vec::new();

        if result.generation != self.url_state.generation {
            return rows;
        }

        self.url_state.in_flight = false;
        self.url_state.requested_at_ms = None;

        let (fg_window_id, fg_exe_name, fg_raw_title) = match &self.open_foreground {
            Some(fg) => (fg.window_id, fg.exe_name.clone(), fg.raw_title.clone()),
            None => return rows,
        };

        if Some(fg_window_id) != self.url_state.requested_window_id
            || fg_window_id != result.window_id
        {
            return rows;
        }

        if !self.page_addresses || self.is_locked {
            return rows;
        }

        if foreground_excluded(&self.exclusion_rules, &fg_exe_name, &fg_raw_title) {
            return rows;
        }

        let Some(snapshot) = source.foreground_snapshot() else {
            return rows;
        };

        if snapshot.window_id != fg_window_id
            || snapshot.exe_name != fg_exe_name
            || foreground_excluded(&self.exclusion_rules, &snapshot.exe_name, &snapshot.title)
        {
            return rows;
        }

        if result.unavailable {
            if let Some(avail_row) = self.transition_availability(
                now_ms,
                "uia",
                AvailabilityCapability::Url,
                AvailabilityStatus::Unavailable,
            ) {
                rows.push(avail_row);
            }
            if let Some(fg) = self.open_foreground.as_mut() {
                if fg.url.is_some() {
                    let old = fg.clone();
                    rows.push(DesktopActivityRow::foreground(
                        old.start_ms,
                        now_ms.max(old.start_ms),
                        old.app,
                        Some(old.exe_name.clone()),
                        old.title,
                        old.url,
                    ));
                    fg.start_ms = now_ms;
                    fg.url = None;
                }
            }
            return rows;
        }

        if let Some(avail_row) = self.close_availability(now_ms, "uia", AvailabilityCapability::Url)
        {
            rows.push(avail_row);
        }

        let Some(raw_url) = result.url else {
            if let Some(fg) = self.open_foreground.as_mut() {
                if fg.url.is_some() {
                    let old = fg.clone();
                    rows.push(DesktopActivityRow::foreground(
                        old.start_ms,
                        now_ms.max(old.start_ms),
                        old.app,
                        Some(old.exe_name.clone()),
                        old.title,
                        old.url,
                    ));
                    fg.start_ms = now_ms;
                    fg.url = None;
                }
            }
            return rows;
        };

        let Some(norm_url) = normalize_url(&raw_url) else {
            if let Some(fg) = self.open_foreground.as_mut() {
                if fg.url.is_some() {
                    let old = fg.clone();
                    rows.push(DesktopActivityRow::foreground(
                        old.start_ms,
                        now_ms.max(old.start_ms),
                        old.app,
                        Some(old.exe_name.clone()),
                        old.title,
                        old.url,
                    ));
                    fg.start_ms = now_ms;
                    fg.url = None;
                }
            }
            return rows;
        };

        if let Some(fg) = self.open_foreground.as_mut() {
            match &fg.url {
                None => {
                    let old = fg.clone();
                    rows.push(DesktopActivityRow::foreground(
                        old.start_ms,
                        now_ms.max(old.start_ms),
                        old.app,
                        Some(old.exe_name.clone()),
                        old.title,
                        None,
                    ));
                    fg.start_ms = now_ms;
                    fg.url = Some(norm_url);
                }
                Some(existing) => {
                    if existing != &norm_url {
                        let old = fg.clone();
                        rows.push(DesktopActivityRow::foreground(
                            old.start_ms,
                            now_ms.max(old.start_ms),
                            old.app,
                            Some(old.exe_name.clone()),
                            old.title,
                            Some(existing.clone()),
                        ));
                        fg.start_ms = now_ms;
                        fg.url = Some(norm_url);
                    }
                }
            }
        }

        rows
    }

    fn handle_event<S: DesktopActivitySource + ?Sized>(
        &mut self,
        source: &mut S,
        event: observer_model::desktop_activity::DesktopActivityProbeEvent,
        now_ms: u64,
    ) -> Vec<DesktopActivityRow> {
        use observer_model::desktop_activity::DesktopActivityProbeEvent::*;

        let mut rows = Vec::new();

        match event {
            Foreground {
                window_id,
                app,
                exe_name,
                title,
            } => {
                if self.is_locked {
                    return rows;
                }

                if foreground_excluded(&self.exclusion_rules, &exe_name, &title) {
                    if let Some(old) = self.open_foreground.take() {
                        rows.push(DesktopActivityRow::foreground(
                            old.start_ms,
                            now_ms.max(old.start_ms),
                            old.app,
                            Some(old.exe_name),
                            old.title,
                            old.url,
                        ));
                    }
                    if self.url_state.in_flight {
                        self.url_state.generation += 1;
                        self.url_state.in_flight = false;
                        self.url_state.requested_window_id = None;
                        self.url_state.requested_at_ms = None;
                        source.cancel_url();
                    }
                    return rows;
                }

                if let Some(current) = self.open_foreground.as_ref() {
                    if current.window_id == window_id
                        && current.raw_title == title
                        && current.app == app
                    {
                        return rows;
                    }
                }

                if let Some(old) = self.open_foreground.take() {
                    rows.push(DesktopActivityRow::foreground(
                        old.start_ms,
                        now_ms.max(old.start_ms),
                        old.app,
                        Some(old.exe_name),
                        old.title,
                        old.url,
                    ));
                }

                if self.url_state.in_flight {
                    self.url_state.generation += 1;
                    self.url_state.in_flight = false;
                    self.url_state.requested_window_id = None;
                    self.url_state.requested_at_ms = None;
                    source.cancel_url();
                }

                let title_val = if self.window_titles {
                    Some(title.clone())
                } else {
                    None
                };

                self.open_foreground = Some(OpenForeground {
                    start_ms: now_ms,
                    window_id,
                    app,
                    exe_name: exe_name.clone(),
                    raw_title: title,
                    title: title_val,
                    url: None,
                });

                if let Some(avail) =
                    self.close_availability(now_ms, "winevent", AvailabilityCapability::Foreground)
                {
                    rows.push(avail);
                }

                let is_browser = exe_name == "msedge.exe" || exe_name == "chrome.exe";
                if self.page_addresses && !self.is_locked {
                    if is_browser {
                        self.url_state.in_flight = true;
                        self.url_state.requested_window_id = Some(window_id);
                        self.url_state.requested_at_ms = Some(now_ms);
                        self.url_state.last_poll_at_ms = Some(now_ms);
                        source.request_url(DesktopActivityUrlRequest {
                            generation: self.url_state.generation,
                            window_id,
                            exe_name,
                        });
                    } else if let Some(avail_row) = self.transition_availability(
                        now_ms,
                        "uia",
                        AvailabilityCapability::Url,
                        AvailabilityStatus::Unavailable,
                    ) {
                        rows.push(avail_row);
                    }
                }
            }
            TitleChanged { window_id, title } => {
                if self.is_locked {
                    return rows;
                }

                let Some(current) = self.open_foreground.as_mut() else {
                    return rows;
                };

                if current.window_id != window_id {
                    return rows;
                }

                if current.raw_title == title {
                    return rows;
                }

                if foreground_excluded(&self.exclusion_rules, &current.exe_name, &title) {
                    let old = self.open_foreground.take().unwrap();
                    rows.push(DesktopActivityRow::foreground(
                        old.start_ms,
                        now_ms.max(old.start_ms),
                        old.app,
                        Some(old.exe_name),
                        old.title,
                        old.url,
                    ));
                    if self.url_state.in_flight {
                        self.url_state.generation += 1;
                        self.url_state.in_flight = false;
                        self.url_state.requested_window_id = None;
                        self.url_state.requested_at_ms = None;
                        source.cancel_url();
                    }
                    return rows;
                }

                let old = self.open_foreground.take().unwrap();
                rows.push(DesktopActivityRow::foreground(
                    old.start_ms,
                    now_ms.max(old.start_ms),
                    old.app.clone(),
                    Some(old.exe_name.clone()),
                    old.title,
                    old.url,
                ));

                if self.url_state.in_flight {
                    self.url_state.generation += 1;
                    self.url_state.in_flight = false;
                    self.url_state.requested_window_id = None;
                    self.url_state.requested_at_ms = None;
                    source.cancel_url();
                }

                let title_val = if self.window_titles {
                    Some(title.clone())
                } else {
                    None
                };

                let is_browser = old.exe_name == "msedge.exe" || old.exe_name == "chrome.exe";

                self.open_foreground = Some(OpenForeground {
                    start_ms: now_ms,
                    window_id,
                    app: old.app,
                    exe_name: old.exe_name.clone(),
                    raw_title: title,
                    title: title_val,
                    url: None,
                });

                if self.page_addresses && !self.is_locked {
                    if is_browser {
                        self.url_state.in_flight = true;
                        self.url_state.requested_window_id = Some(window_id);
                        self.url_state.requested_at_ms = Some(now_ms);
                        self.url_state.last_poll_at_ms = Some(now_ms);
                        source.request_url(DesktopActivityUrlRequest {
                            generation: self.url_state.generation,
                            window_id,
                            exe_name: old.exe_name,
                        });
                    } else if let Some(avail_row) = self.transition_availability(
                        now_ms,
                        "uia",
                        AvailabilityCapability::Url,
                        AvailabilityStatus::Unavailable,
                    ) {
                        rows.push(avail_row);
                    }
                }
            }
            ActivityStateChanged(new_state) => {
                let state_changed = match &self.open_activity {
                    Some(a) => a.state != new_state,
                    None => true,
                };

                if state_changed {
                    if let Some(old) = self.open_activity.take() {
                        rows.push(DesktopActivityRow::activity(
                            old.start_ms,
                            now_ms.max(old.start_ms),
                            old.state,
                        ));
                    }
                    self.open_activity = Some(OpenActivity {
                        start_ms: now_ms,
                        state: new_state,
                    });
                }

                if new_state == ActivityState::Locked {
                    self.is_locked = true;
                    if let Some(old) = self.open_foreground.take() {
                        rows.push(DesktopActivityRow::foreground(
                            old.start_ms,
                            now_ms.max(old.start_ms),
                            old.app,
                            Some(old.exe_name),
                            old.title,
                            old.url,
                        ));
                    }
                    if self.url_state.in_flight {
                        self.url_state.generation += 1;
                        self.url_state.in_flight = false;
                        self.url_state.requested_window_id = None;
                        self.url_state.requested_at_ms = None;
                        source.cancel_url();
                    }
                } else {
                    self.is_locked = false;
                }

                if let Some(avail) =
                    self.close_availability(now_ms, "winevent", AvailabilityCapability::Idle)
                {
                    rows.push(avail);
                }
            }
            ForegroundUnavailable => {
                if let Some(old) = self.open_foreground.take() {
                    rows.push(DesktopActivityRow::foreground(
                        old.start_ms,
                        now_ms.max(old.start_ms),
                        old.app,
                        Some(old.exe_name),
                        old.title,
                        old.url,
                    ));
                }
                if self.url_state.in_flight {
                    self.url_state.generation += 1;
                    self.url_state.in_flight = false;
                    self.url_state.requested_window_id = None;
                    self.url_state.requested_at_ms = None;
                    source.cancel_url();
                }

                if let Some(avail) = self.transition_availability(
                    now_ms,
                    "winevent",
                    AvailabilityCapability::Foreground,
                    AvailabilityStatus::Unavailable,
                ) {
                    rows.push(avail);
                }
            }
            IdleUnavailable => {
                if let Some(old) = self.open_activity.take() {
                    rows.push(DesktopActivityRow::activity(
                        old.start_ms,
                        now_ms.max(old.start_ms),
                        old.state,
                    ));
                }
                if let Some(avail) = self.transition_availability(
                    now_ms,
                    "winevent",
                    AvailabilityCapability::Idle,
                    AvailabilityStatus::Unavailable,
                ) {
                    rows.push(avail);
                }
            }
        }

        rows
    }

    fn transition_availability(
        &mut self,
        now_ms: u64,
        method: &str,
        capability: AvailabilityCapability,
        status: AvailabilityStatus,
    ) -> Option<DesktopActivityRow> {
        let key = (capability, method.to_string());
        if let Some(existing) = self.open_availabilities.get(&key) {
            if existing.status == status {
                return None;
            }
        }

        self.open_availabilities.insert(
            key,
            OpenAvailability {
                start_ms: now_ms,
                status,
            },
        );

        Some(DesktopActivityRow::availability(
            now_ms, now_ms, method, capability, status,
        ))
    }

    fn close_availability(
        &mut self,
        _now_ms: u64,
        method: &str,
        capability: AvailabilityCapability,
    ) -> Option<DesktopActivityRow> {
        let key = (capability, method.to_string());
        self.open_availabilities.remove(&key);
        None
    }

    /// Split open intervals across a rotation boundary.
    pub fn on_boundary(&mut self, boundary_ms: u64) -> Vec<DesktopActivityRow> {
        let mut rows = Vec::new();

        if let Some(fg) = self.open_foreground.take() {
            rows.push(DesktopActivityRow::foreground(
                fg.start_ms,
                boundary_ms.max(fg.start_ms),
                fg.app.clone(),
                Some(fg.exe_name.clone()),
                fg.title.clone(),
                fg.url.clone(),
            ));
            self.open_foreground = Some(OpenForeground {
                start_ms: boundary_ms,
                window_id: fg.window_id,
                app: fg.app,
                exe_name: fg.exe_name,
                raw_title: fg.raw_title,
                title: fg.title,
                url: fg.url,
            });
        }

        if let Some(act) = self.open_activity.take() {
            rows.push(DesktopActivityRow::activity(
                act.start_ms,
                boundary_ms.max(act.start_ms),
                act.state,
            ));
            self.open_activity = Some(OpenActivity {
                start_ms: boundary_ms,
                state: act.state,
            });
        }

        rows
    }

    /// Close all open intervals at stop or pause.
    pub fn close_all<S: DesktopActivitySource + ?Sized>(
        &mut self,
        source: &mut S,
        now_ms: u64,
    ) -> Vec<DesktopActivityRow> {
        self.url_state.generation += 1;
        self.url_state.in_flight = false;
        self.url_state.requested_window_id = None;
        self.url_state.requested_at_ms = None;
        source.cancel_url();

        let mut rows = Vec::new();

        if let Some(fg) = self.open_foreground.take() {
            rows.push(DesktopActivityRow::foreground(
                fg.start_ms,
                now_ms.max(fg.start_ms),
                fg.app,
                Some(fg.exe_name),
                fg.title,
                fg.url,
            ));
        }

        if let Some(act) = self.open_activity.take() {
            rows.push(DesktopActivityRow::activity(
                act.start_ms,
                now_ms.max(act.start_ms),
                act.state,
            ));
        }

        for (key, avail) in self.open_availabilities.drain() {
            rows.push(DesktopActivityRow::availability(
                avail.start_ms,
                now_ms.max(avail.start_ms),
                &key.1,
                key.0,
                avail.status,
            ));
        }

        rows
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use observer_model::desktop_activity::{DesktopActivityProbeEvent, ForegroundSnapshot};
    use observer_model::{SourceError, SourceState};

    #[derive(Default)]
    struct FakeDesktopActivityProbe {
        running: bool,
        events: Vec<DesktopActivityProbeEvent>,
        last_request: Option<DesktopActivityUrlRequest>,
        next_result: Option<DesktopActivityUrlResult>,
        reap_failed: bool,
        worker_count: usize,
        cancels: usize,
        snapshot: Option<ForegroundSnapshot>,
    }

    impl DesktopActivitySource for FakeDesktopActivityProbe {
        fn start(&mut self) -> Result<(), SourceError> {
            self.running = true;
            Ok(())
        }

        fn stop(&mut self) {
            self.running = false;
        }

        fn state(&self) -> SourceState {
            if self.running {
                SourceState::Active
            } else {
                SourceState::Inactive
            }
        }

        fn poll_events(&mut self) -> Vec<DesktopActivityProbeEvent> {
            std::mem::take(&mut self.events)
        }

        fn request_url(&mut self, request: DesktopActivityUrlRequest) {
            if self.reap_failed {
                return;
            }
            self.last_request = Some(request);
            self.worker_count = 1;
        }

        fn poll_url_result(&mut self) -> Option<DesktopActivityUrlResult> {
            self.next_result.take()
        }

        fn cancel_url(&mut self) {
            self.worker_count = 0;
            self.cancels += 1;
        }

        fn foreground_snapshot(&mut self) -> Option<ForegroundSnapshot> {
            if let Some(snap) = &self.snapshot {
                return Some(snap.clone());
            }
            if let Some(req) = &self.last_request {
                return Some(ForegroundSnapshot {
                    window_id: req.window_id,
                    app: req.exe_name.clone(),
                    exe_name: req.exe_name.clone(),
                    title: "Browser Title".into(),
                });
            }
            None
        }
    }

    fn test_rules() -> ExclusionRules {
        ExclusionRules::default()
    }

    #[test]
    fn result_queued_in_the_same_poll_as_a_lock_is_not_stored() {
        let mut scheduler =
            DesktopActivityScheduler::new(true, true, test_rules(), "testart".into());
        let mut probe = FakeDesktopActivityProbe::default();

        probe.events.push(DesktopActivityProbeEvent::Foreground {
            window_id: 10,
            app: "Edge".into(),
            exe_name: "msedge.exe".into(),
            title: "Bank Page".into(),
        });
        scheduler.poll_and_step(&mut probe, 1000);
        assert!(scheduler.url_state.in_flight);
        let gen = scheduler.url_state.generation;

        // In a single poll, lock event arrives AND queued result is in hand
        probe
            .events
            .push(DesktopActivityProbeEvent::ActivityStateChanged(
                ActivityState::Locked,
            ));
        probe.next_result = Some(DesktopActivityUrlResult {
            generation: gen,
            window_id: 10,
            url: Some("https://bank.example.com/account".into()),
            unavailable: false,
        });

        let rows = scheduler.poll_and_step(&mut probe, 1100);
        // Lock event closed the foreground interval before URL result was considered
        assert!(scheduler.is_locked);
        assert!(scheduler.open_foreground.is_none());
        assert_eq!(probe.cancels, 1);

        for row in &rows {
            if let observer_model::desktop_activity::DesktopActivityPayload::Foreground(p) =
                &row.payload
            {
                assert!(p.url.is_none(), "url must not be published on lock");
            }
        }
    }

    #[test]
    fn timeout_emits_url_unavailable_and_clears_url() {
        let mut scheduler =
            DesktopActivityScheduler::new(true, true, test_rules(), "testart".into());
        let mut probe = FakeDesktopActivityProbe::default();

        probe.events.push(DesktopActivityProbeEvent::Foreground {
            window_id: 20,
            app: "Edge".into(),
            exe_name: "msedge.exe".into(),
            title: "Slow Page".into(),
        });
        scheduler.poll_and_step(&mut probe, 1000);

        // First admit a URL
        probe.next_result = Some(DesktopActivityUrlResult {
            generation: scheduler.url_state.generation,
            window_id: 20,
            url: Some("https://example.com/page".into()),
            unavailable: false,
        });
        scheduler.poll_and_step(&mut probe, 1100);
        assert_eq!(
            scheduler.open_foreground.as_ref().unwrap().url,
            Some("https://example.com/page".into())
        );

        // 4s cadence triggers re-read at 5100ms
        scheduler.poll_and_step(&mut probe, 5100);
        assert!(scheduler.url_state.in_flight);

        // Advance 2100ms past deadline (7200ms) with a hung result arriving on timeout poll
        probe.next_result = Some(DesktopActivityUrlResult {
            generation: scheduler.url_state.generation,
            window_id: 20,
            url: Some("https://example.com/late".into()),
            unavailable: false,
        });
        let rows = scheduler.poll_and_step(&mut probe, 7200);

        // Timeout emitted availability unavailable and split open foreground to url: None
        let avail = rows
            .iter()
            .find(|r| r.kind == "availability")
            .expect("must emit availability row on timeout");
        assert_eq!(avail.method, "uia");
        if let observer_model::desktop_activity::DesktopActivityPayload::Availability(p) =
            &avail.payload
        {
            assert_eq!(p.capability, "url");
            assert_eq!(p.status, "unavailable");
        }

        assert_eq!(scheduler.open_foreground.as_ref().unwrap().url, None);
        assert_eq!(probe.cancels, 1);
    }

    #[test]
    fn unavailable_result_splits_url_to_none() {
        let mut scheduler =
            DesktopActivityScheduler::new(true, true, test_rules(), "testart".into());
        let mut probe = FakeDesktopActivityProbe::default();

        probe.events.push(DesktopActivityProbeEvent::Foreground {
            window_id: 30,
            app: "Edge".into(),
            exe_name: "msedge.exe".into(),
            title: "Navigating".into(),
        });
        scheduler.poll_and_step(&mut probe, 1000);

        probe.next_result = Some(DesktopActivityUrlResult {
            generation: scheduler.url_state.generation,
            window_id: 30,
            url: Some("https://example.com/old".into()),
            unavailable: false,
        });
        scheduler.poll_and_step(&mut probe, 1100);
        assert!(scheduler.open_foreground.as_ref().unwrap().url.is_some());

        // 4s later, probe reports unavailable
        scheduler.poll_and_step(&mut probe, 5200);
        probe.next_result = Some(DesktopActivityUrlResult {
            generation: scheduler.url_state.generation,
            window_id: 30,
            url: None,
            unavailable: true,
        });
        let rows = scheduler.poll_and_step(&mut probe, 5300);

        // Foreground interval with old URL is closed, new interval has url: None
        let fg_closed = rows.iter().find(|r| r.kind == "foreground").unwrap();
        if let observer_model::desktop_activity::DesktopActivityPayload::Foreground(p) =
            &fg_closed.payload
        {
            assert_eq!(p.url, Some("https://example.com/old".into()));
            assert_eq!(p.app_id, Some("msedge.exe".into()));
        }
        assert_eq!(scheduler.open_foreground.as_ref().unwrap().url, None);
    }

    #[test]
    fn firefox_with_page_addresses_on_emits_url_unavailable_and_does_not_request_url() {
        let mut scheduler =
            DesktopActivityScheduler::new(true, true, test_rules(), "testart".into());
        let mut probe = FakeDesktopActivityProbe::default();

        probe.events.push(DesktopActivityProbeEvent::Foreground {
            window_id: 40,
            app: "Firefox".into(),
            exe_name: "firefox.exe".into(),
            title: "Mozilla".into(),
        });

        let rows = scheduler.poll_and_step(&mut probe, 1000);
        assert!(
            probe.last_request.is_none(),
            "must not request URL for firefox"
        );

        let avail = rows
            .iter()
            .find(|r| r.kind == "availability")
            .expect("must emit availability row");
        assert_eq!(avail.method, "uia");
        if let observer_model::desktop_activity::DesktopActivityPayload::Availability(p) =
            &avail.payload
        {
            assert_eq!(p.capability, "url");
            assert_eq!(p.status, "unavailable");
        }

        // Deduped: second poll does not re-emit
        let rows2 = scheduler.poll_and_step(&mut probe, 5100);
        assert!(rows2.is_empty());
        assert!(probe.last_request.is_none());
    }

    #[test]
    fn titles_turned_off_drops_an_in_flight_result() {
        let mut scheduler =
            DesktopActivityScheduler::new(true, true, test_rules(), "testart".into());
        let mut probe = FakeDesktopActivityProbe::default();

        probe.events.push(DesktopActivityProbeEvent::Foreground {
            window_id: 50,
            app: "Edge".into(),
            exe_name: "msedge.exe".into(),
            title: "Private Doc".into(),
        });
        scheduler.poll_and_step(&mut probe, 1000);
        let gen = scheduler.url_state.generation;

        // Turn window titles off during in-flight read
        scheduler.set_preferences(&mut probe, false, true, 1100);
        assert_ne!(scheduler.url_state.generation, gen);
        assert_eq!(probe.cancels, 1);

        // Result with stale generation arrives
        probe.next_result = Some(DesktopActivityUrlResult {
            generation: gen,
            window_id: 50,
            url: Some("https://example.com/doc".into()),
            unavailable: false,
        });
        let rows = scheduler.poll_and_step(&mut probe, 1200);
        assert!(rows.is_empty());
        assert_eq!(scheduler.open_foreground.as_ref().unwrap().url, None);
    }

    #[test]
    fn excluded_same_hwnd_then_fresh_foreground_admits_new_identity_and_does_not_keep_withheld_title(
    ) {
        let mut rules = ExclusionRules::default();
        rules.title_patterns.push("Secret".into());
        let mut scheduler = DesktopActivityScheduler::new(true, true, rules, "testart".into());
        let mut probe = FakeDesktopActivityProbe::default();

        // 1. Excluded title on HWND 60
        probe.events.push(DesktopActivityProbeEvent::Foreground {
            window_id: 60,
            app: "Edge".into(),
            exe_name: "msedge.exe".into(),
            title: "Secret Document".into(),
        });
        let rows = scheduler.poll_and_step(&mut probe, 1000);
        assert!(rows.is_empty());
        assert!(
            scheduler.open_foreground.is_none(),
            "must withhold excluded window"
        );

        // 2. TitleChanged on withheld window writes no row
        probe.events.push(DesktopActivityProbeEvent::TitleChanged {
            window_id: 60,
            title: "Still Secret".into(),
        });
        let rows = scheduler.poll_and_step(&mut probe, 1100);
        assert!(rows.is_empty());
        assert!(scheduler.open_foreground.is_none());

        // 3. Same HWND 60 now has an admitted title -> fresh Foreground event
        probe.events.push(DesktopActivityProbeEvent::Foreground {
            window_id: 60,
            app: "Edge".into(),
            exe_name: "msedge.exe".into(),
            title: "Public Page".into(),
        });
        let rows = scheduler.poll_and_step(&mut probe, 1200);
        assert!(rows.is_empty());
        let fg = scheduler.open_foreground.as_ref().unwrap();
        assert_eq!(fg.window_id, 60);
        assert_eq!(fg.raw_title, "Public Page");
        assert_eq!(fg.title, Some("Public Page".into()));
    }

    #[test]
    fn unchanged_admitted_url_does_not_split() {
        let mut scheduler =
            DesktopActivityScheduler::new(true, true, test_rules(), "testart".into());
        let mut probe = FakeDesktopActivityProbe::default();

        probe.events.push(DesktopActivityProbeEvent::Foreground {
            window_id: 70,
            app: "Edge".into(),
            exe_name: "msedge.exe".into(),
            title: "Stable Page".into(),
        });
        scheduler.poll_and_step(&mut probe, 1000);

        probe.next_result = Some(DesktopActivityUrlResult {
            generation: scheduler.url_state.generation,
            window_id: 70,
            url: Some("https://example.com/stable".into()),
            unavailable: false,
        });
        scheduler.poll_and_step(&mut probe, 1100);
        let start_ms = scheduler.open_foreground.as_ref().unwrap().start_ms;

        // 4s cadence returns the exact same URL
        scheduler.poll_and_step(&mut probe, 5100);
        probe.next_result = Some(DesktopActivityUrlResult {
            generation: scheduler.url_state.generation,
            window_id: 70,
            url: Some("https://example.com/stable".into()),
            unavailable: false,
        });
        let rows = scheduler.poll_and_step(&mut probe, 5200);

        assert!(rows.is_empty());
        assert_eq!(
            scheduler.open_foreground.as_ref().unwrap().start_ms,
            start_ms
        );
    }

    #[test]
    fn same_title_document_url_change_and_unavailable_to_available_recovery_update_within_cadence_and_deadline(
    ) {
        let mut scheduler =
            DesktopActivityScheduler::new(true, true, test_rules(), "testart".into());
        let mut probe = FakeDesktopActivityProbe::default();

        // Start with window
        probe.events.push(DesktopActivityProbeEvent::Foreground {
            window_id: 80,
            app: "Chrome".into(),
            exe_name: "chrome.exe".into(),
            title: "Dynamic App".into(),
        });
        scheduler.poll_and_step(&mut probe, 1000);

        // Initially unavailable
        probe.next_result = Some(DesktopActivityUrlResult {
            generation: scheduler.url_state.generation,
            window_id: 80,
            url: None,
            unavailable: true,
        });
        scheduler.poll_and_step(&mut probe, 1100);
        assert_eq!(scheduler.open_foreground.as_ref().unwrap().url, None);

        // 4s cadence re-requests, returns loaded URL
        scheduler.poll_and_step(&mut probe, 5200);
        probe.next_result = Some(DesktopActivityUrlResult {
            generation: scheduler.url_state.generation,
            window_id: 80,
            url: Some("https://example.com/app/view1".into()),
            unavailable: false,
        });
        let rows = scheduler.poll_and_step(&mut probe, 5300);
        assert_eq!(rows.len(), 1);
        assert_eq!(
            scheduler.open_foreground.as_ref().unwrap().url,
            Some("https://example.com/app/view1".into())
        );

        // Another 4s cadence, URL changes to view2 with same title
        scheduler.poll_and_step(&mut probe, 9400);
        probe.next_result = Some(DesktopActivityUrlResult {
            generation: scheduler.url_state.generation,
            window_id: 80,
            url: Some("https://example.com/app/view2".into()),
            unavailable: false,
        });
        let rows = scheduler.poll_and_step(&mut probe, 9500);
        assert_eq!(rows.len(), 1);
        if let observer_model::desktop_activity::DesktopActivityPayload::Foreground(p) =
            &rows[0].payload
        {
            assert_eq!(p.url, Some("https://example.com/app/view1".into()));
        }
        assert_eq!(
            scheduler.open_foreground.as_ref().unwrap().url,
            Some("https://example.com/app/view2".into())
        );
    }

    #[test]
    fn url_dropped_when_admit_snapshot_differs_or_excluded() {
        let mut rules = ExclusionRules::default();
        rules.title_patterns.push("Secret".into());
        let mut scheduler = DesktopActivityScheduler::new(true, true, rules, "testart".into());
        let mut probe = FakeDesktopActivityProbe::default();

        // 1. Cached open window A
        probe.events.push(DesktopActivityProbeEvent::Foreground {
            window_id: 100,
            app: "Edge".into(),
            exe_name: "msedge.exe".into(),
            title: "Normal Page".into(),
        });
        scheduler.poll_and_step(&mut probe, 1000);
        let gen = scheduler.url_state.generation;

        // Snapshot has a different window ID (B)
        probe.snapshot = Some(ForegroundSnapshot {
            window_id: 200,
            app: "Edge".into(),
            exe_name: "msedge.exe".into(),
            title: "Normal Page".into(),
        });
        probe.next_result = Some(DesktopActivityUrlResult {
            generation: gen,
            window_id: 100,
            url: Some("https://example.com/should_drop".into()),
            unavailable: false,
        });
        let rows = scheduler.poll_and_step(&mut probe, 1100);
        assert!(rows.is_empty());
        assert_eq!(scheduler.open_foreground.as_ref().unwrap().url, None);

        // Reset in_flight state by advancing time for next cadence request
        scheduler.url_state.in_flight = false;
        scheduler.poll_and_step(&mut probe, 5200);
        let gen2 = scheduler.url_state.generation;

        // Snapshot has window A, but excluded title
        probe.snapshot = Some(ForegroundSnapshot {
            window_id: 100,
            app: "Edge".into(),
            exe_name: "msedge.exe".into(),
            title: "Secret Window".into(),
        });
        probe.next_result = Some(DesktopActivityUrlResult {
            generation: gen2,
            window_id: 100,
            url: Some("https://example.com/should_drop_2".into()),
            unavailable: false,
        });
        let rows2 = scheduler.poll_and_step(&mut probe, 5300);
        assert!(rows2.is_empty());
        assert_eq!(scheduler.open_foreground.as_ref().unwrap().url, None);
    }
}
