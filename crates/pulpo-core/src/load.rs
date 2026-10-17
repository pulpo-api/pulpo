//! A light load test: N virtual users run the same requests (with their scripts, so a
//! login chains into the rest) in a loop for M seconds. Each user has its own in-memory
//! session; nothing is written to the workspace.

use crate::collection::RequestEntry;
use crate::run::{self, RunOptions, Session};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Most virtual users allowed: this is a quick check, not a load generator farm.
pub const MAX_USERS: usize = 200;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoadOptions {
    pub users: usize,
    pub duration_secs: u64,
    /// Seconds over which users start, one after another (0: all at once).
    #[serde(default)]
    pub ramp_up_secs: u64,
    /// Data rows: user N runs with row N (wrapping), as `{{name}}` and `pm.iterationData`.
    #[serde(default)]
    pub data: Vec<crate::data::Row>,
}

/// Figures for one request (or the whole test).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoadStats {
    pub name: String,
    pub count: usize,
    /// Could not be sent, or a test or script failed.
    pub failed: usize,
    pub min_ms: u64,
    pub avg_ms: u64,
    pub p50_ms: u64,
    pub p90_ms: u64,
    pub p95_ms: u64,
    pub p99_ms: u64,
    pub max_ms: u64,
    pub per_second: f64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoadSummary {
    pub users: usize,
    pub elapsed_ms: u64,
    pub total: LoadStats,
    pub requests: Vec<LoadStats>,
    /// The first few distinct failure messages, to see what went wrong.
    pub errors: Vec<String>,
}

/// Where a test is, for a progress line.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoadProgress {
    pub elapsed_secs: u64,
    pub done: u64,
    pub failed: u64,
}

struct Sample {
    request: usize,
    ms: u64,
    ok: bool,
}

/// The `q` quantile (0..=1) of sorted values, nearest-rank.
fn percentile(sorted: &[u64], q: f64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let rank = (q * sorted.len() as f64).ceil() as usize;
    sorted[rank.clamp(1, sorted.len()) - 1]
}

fn stats(name: &str, samples: &mut [u64], failed: usize, seconds: f64) -> LoadStats {
    samples.sort_unstable();
    let count = samples.len();
    LoadStats {
        name: name.to_string(),
        count,
        failed,
        min_ms: samples.first().copied().unwrap_or(0),
        avg_ms: if count == 0 {
            0
        } else {
            samples.iter().sum::<u64>() / count as u64
        },
        p50_ms: percentile(samples, 0.50),
        p90_ms: percentile(samples, 0.90),
        p95_ms: percentile(samples, 0.95),
        p99_ms: percentile(samples, 0.99),
        max_ms: samples.last().copied().unwrap_or(0),
        per_second: if seconds > 0.0 {
            (count as f64 / seconds * 10.0).round() / 10.0
        } else {
            0.0
        },
    }
}

/// Runs the test, calling `progress` about once a second. `stop` ends it early.
pub fn run_load(
    root: &Path,
    requests: &[RequestEntry],
    session: &Session,
    options: &LoadOptions,
    stop: Arc<AtomicBool>,
    progress: &(dyn Fn(LoadProgress) + Sync),
) -> Result<LoadSummary, String> {
    if requests.is_empty() {
        return Err("nothing to run".into());
    }
    let users = options.users.clamp(1, MAX_USERS);
    let duration = Duration::from_secs(options.duration_secs.max(1));
    let ramp = Duration::from_secs(options.ramp_up_secs.min(options.duration_secs));
    let samples = Arc::new(Mutex::new(Vec::<Sample>::new()));
    let errors = Arc::new(Mutex::new(Vec::<String>::new()));
    let done = Arc::new(AtomicU64::new(0));
    let failed = Arc::new(AtomicU64::new(0));
    let started = Instant::now();
    let deadline = started + duration;
    let data = &options.data;

    std::thread::scope(|scope| -> Result<(), String> {
        for user in 0..users {
            let mut session = session.detached();
            let root: PathBuf = root.to_path_buf();
            let samples = samples.clone();
            let errors = errors.clone();
            let done = done.clone();
            let failed = failed.clone();
            let stop = stop.clone();
            let delay = if users > 1 {
                ramp.mul_f64(user as f64 / users as f64)
            } else {
                Duration::ZERO
            };
            scope.spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(e) => {
                        errors.lock().unwrap().push(e.to_string());
                        return;
                    }
                };
                std::thread::sleep(delay);
                let options = RunOptions {
                    data: data
                        .get(user % data.len().max(1))
                        .cloned()
                        .unwrap_or_default(),
                    ..Default::default()
                };
                'run: loop {
                    for (index, entry) in requests.iter().enumerate() {
                        if Instant::now() >= deadline || stop.load(Ordering::Relaxed) {
                            break 'run;
                        }
                        let t = Instant::now();
                        let report =
                            runtime.block_on(run::run_in(&root, entry, &mut session, &options));
                        let ms = report
                            .response
                            .as_ref()
                            .map(|r| r.duration_ms)
                            .unwrap_or_else(|| t.elapsed().as_millis() as u64);
                        let ok = report.passed();
                        done.fetch_add(1, Ordering::Relaxed);
                        if !ok {
                            failed.fetch_add(1, Ordering::Relaxed);
                            let message = report
                                .error
                                .as_ref()
                                .map(|e| e.to_string())
                                .or_else(|| report.script_errors.first().cloned())
                                .or_else(|| {
                                    report.tests.iter().find(|x| !x.passed).map(|x| {
                                        format!(
                                            "{}: {}",
                                            x.name,
                                            x.error.as_deref().unwrap_or("failed")
                                        )
                                    })
                                })
                                .unwrap_or_else(|| "failed".into());
                            let mut errors = errors.lock().unwrap();
                            let message = format!("{}: {message}", entry.name);
                            if errors.len() < 10 && !errors.contains(&message) {
                                errors.push(message);
                            }
                        }
                        samples.lock().unwrap().push(Sample {
                            request: index,
                            ms,
                            ok,
                        });
                    }
                }
            });
        }
        // Progress, once a second, while the users work.
        while Instant::now() < deadline && !stop.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(250));
            progress(LoadProgress {
                elapsed_secs: started.elapsed().as_secs(),
                done: done.load(Ordering::Relaxed),
                failed: failed.load(Ordering::Relaxed),
            });
        }
        Ok(())
    })?;

    let elapsed = started.elapsed();
    let seconds = elapsed.as_secs_f64();
    let samples = std::mem::take(&mut *samples.lock().unwrap());
    let mut per_request: Vec<(Vec<u64>, usize)> = vec![(Vec::new(), 0); requests.len()];
    let mut all = Vec::with_capacity(samples.len());
    let mut all_failed = 0;
    for sample in &samples {
        per_request[sample.request].0.push(sample.ms);
        all.push(sample.ms);
        if !sample.ok {
            per_request[sample.request].1 += 1;
            all_failed += 1;
        }
    }
    let requests = requests
        .iter()
        .zip(per_request.iter_mut())
        .map(|(entry, (ms, failed))| stats(&entry.name, ms, *failed, seconds))
        .collect();
    let errors = errors.lock().unwrap().clone();
    Ok(LoadSummary {
        users,
        elapsed_ms: elapsed.as_millis() as u64,
        total: stats("All requests", &mut all, all_failed, seconds),
        requests,
        errors,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collection::{init_workspace, Scripts};
    use crate::environment::MemoryStore;
    use crate::http::HttpRequest;
    use crate::{HttpMethod, Protocol};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn computes_nearest_rank_percentiles() {
        let mut values: Vec<u64> = (1..=100).collect();
        let s = stats("x", &mut values, 0, 10.0);
        assert_eq!(
            (s.min_ms, s.p50_ms, s.p90_ms, s.p99_ms, s.max_ms, s.avg_ms),
            (1, 50, 90, 99, 100, 50)
        );
        assert_eq!(s.per_second, 10.0);
        assert_eq!(percentile(&[], 0.5), 0);
        assert_eq!(percentile(&[7], 0.99), 7);
    }

    #[test]
    fn several_users_loop_over_the_requests() {
        // A tiny server on its own thread that answers every connection with 200.
        let rt = tokio::runtime::Runtime::new().unwrap();
        let listener = rt
            .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
            .unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        rt.spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                tokio::spawn(async move {
                    let mut buffer = vec![0u8; 4096];
                    let _ = socket.read(&mut buffer).await;
                    let _ = socket
                        .write_all(
                            b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok",
                        )
                        .await;
                });
            }
        });

        let ws = tempfile::tempdir().unwrap();
        init_workspace(ws.path(), "Demo").unwrap();
        let entry = |name: &str, test: &str| RequestEntry {
            id: format!("{name}.pulpo.yaml"),
            name: name.into(),
            protocol: Protocol::Http,
            request: HttpRequest::new(HttpMethod::Get, format!("{base}/{name}")),
            scripts: Scripts {
                pre: String::new(),
                test: test.into(),
            },
        };
        let requests = vec![
            entry(
                "ok",
                "pm.test('200', () => pm.response.to.have.status(200));",
            ),
            entry(
                "bad",
                "pm.test('201', () => pm.response.to.have.status(201));",
            ),
        ];
        let session = Session::open(ws.path(), None, &MemoryStore::default(), true).unwrap();
        let ticks = AtomicU64::new(0);
        let summary = run_load(
            ws.path(),
            &requests,
            &session,
            &LoadOptions {
                users: 3,
                duration_secs: 1,
                ramp_up_secs: 0,
                data: Vec::new(),
            },
            Arc::new(AtomicBool::new(false)),
            &|_| {
                ticks.fetch_add(1, Ordering::Relaxed);
            },
        )
        .unwrap();
        assert_eq!(summary.users, 3);
        assert!(summary.total.count >= 6, "{summary:?}");
        assert_eq!(summary.requests[0].failed, 0);
        assert_eq!(summary.requests[1].failed, summary.requests[1].count);
        assert_eq!(summary.errors.len(), 1, "{:?}", summary.errors);
        assert!(ticks.load(Ordering::Relaxed) > 0);
        assert!(
            !ws.path().join(".pulpo").exists(),
            "a load test writes nothing to the workspace"
        );
    }
}
