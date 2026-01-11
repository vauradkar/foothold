//! Foothold: A session state management crate for tracking successful, failed,
//! and skipped tasks.
//!
//! This crate provides utilities for managing session state, tracking progress,
//! and persisting results for batch processing tasks.
//!
//! # Features
//!
//! - Track successful, failed, and skipped tasks
//! - Persist session state to disk
//! - Compute statistics for current and previous sessions
//! - Thread-safe file writing
//!
//! # Example
//!
//! ```
//! use foothold::{Foothold, Stats};
//!
//! /// Helper function to simulates running tasks with possible failures and
//! /// resumes from previous state.
//! fn run_tasks(
//!     tasks: &[String],
//!     foothold: &mut Foothold<String, (), ()>,
//!     fake_failure: bool) {
//!     for (index, task) in tasks.iter().enumerate() {
//!         let task = task.to_string();
//!         if foothold.is_successful(&task) {
//!             println!("skipping {task}");
//!             foothold.mark_skipped(task).unwrap();
//!             continue;
//!         } else if fake_failure && foothold.is_failed(&task) {
//!             // If previously failed, skip (or retry)
//!             println!("skipping {task}");
//!             foothold.mark_skipped(task).unwrap();
//!             continue;
//!         }
//!
//!         if fake_failure && index % 2 == 1 {
//!             println!("failing {task}");
//!             foothold.mark_failed(task, ()).unwrap();
//!         } else {
//!             println!("completing {task}");
//!             foothold.mark_successful(task, ()).unwrap();
//!         }
//!     }
//! }
//!
//! # let successful_file = tempfile::NamedTempFile::new().unwrap();
//! # let failed_file = tempfile::NamedTempFile::new().unwrap();
//! # let success_log = successful_file.path();
//! # let failed_log = failed_file.path();
//! let mut tasks = vec![
//!     "task1".to_string(),
//!     "task2".to_string(),
//!     "task3".to_string(),
//! ];
//!
//! /// First run. Assume this session may have failures or gets interrupted.
//! {
//!     let mut foothold =
//!         Foothold::<String, (), ()>::new(success_log, failed_log, tasks.len()).unwrap();
//!     run_tasks(&tasks, &mut foothold, true);
//!     let stats = foothold.stats();
//!     println!("Stats after first run: {}", stats);
//!  #   assert_eq!(stats.session_successful, 2);
//!  #   assert_eq!(stats.session_failed, 1);
//!  #   assert_eq!(stats.session_skipped, 0);
//! }
//!
//! // More tasks get added
//! tasks.push("task4".to_string());
//!
//! /// Second run. Resuming from previous state.
//! {
//!     let mut foothold =
//!         Foothold::<String, (), ()>::new(success_log, failed_log, tasks.len()).unwrap();
//!     run_tasks(&tasks, &mut foothold, false);
//!     let stats = foothold.stats();
//!     println!("Stats after resume: {}", stats);
//!  #   assert_eq!(stats.session_successful, 2);
//!  #   assert_eq!(stats.session_failed, 0);
//!  #   assert_eq!(stats.session_skipped, 2);
//! }
//! ```
//!
//! The above should produce output similar to:
//!
//! ```text
//! completing task1
//! failing task2
//! completing task3
//! Stats after first run: Total: 3, Successful: 2, Failed: 1, Skipped: 0, Elapsed: 59.81µs, Rate: 33287.84 tasks/sec. In previous session: successful: 0, failed: 0
//! skipping task1
//! completing task2
//! skipping task3
//! completing task4
//! Stats after resume: Total: 4, Successful: 2, Failed: 0, Skipped: 2, Elapsed: 23.57µs, Rate: 84670.42 tasks/sec. In previous session: successful: 2, failed: 1
//! ```
//! See struct-level documentation for details.
use std::collections::HashMap;
use std::fmt::Display;
use std::fs::File;
use std::fs::OpenOptions;
use std::io::BufRead;
use std::io::BufReader;
use std::io::BufWriter;
use std::io::Write;
use std::path::Path;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::SystemTime;

use serde::Deserialize;
use serde::Serialize;
use serde_json::from_str;

/// Configuration options for synchronizing batch processing sessions.
pub struct SyncConfig {
    /// Optional duration between two sync operations in seconds.
    interval: Option<u64>,

    /// Number of operations between two sync operations.
    operation_count: Option<u64>,

    /// System time when last time sync was issued.
    last_sync: SystemTime,

    /// operations issues since last sync
    ops_since_sync: u64,
}

impl SyncConfig {
    /// `timed` is the duration between two sync operations in seconds.
    /// `operation_count` is the number of operations between two sync
    /// operations. If both are none or haze zero value, sync is issued for each
    /// operation.
    pub fn create(interval: Option<u64>, mut operation_count: Option<u64>) -> Self {
        if interval.is_none() && operation_count.is_none() {
            operation_count = Some(1);
        }
        if let Some(c) = operation_count
            && c == 0
        {
            operation_count = Some(1);
        }
        Self {
            interval,
            operation_count,
            last_sync: SystemTime::now(),
            ops_since_sync: 0,
        }
    }

    fn should_sync(&mut self) -> bool {
        let sync_from_count = self
            .operation_count
            .as_ref()
            .map(|s| self.ops_since_sync >= *s)
            .unwrap_or(false);

        let sync_from_timeout = self
            .interval
            .as_ref()
            .map(|t| self.last_sync.elapsed().unwrap().as_secs() >= *t)
            .unwrap_or(false);

        if sync_from_count || sync_from_timeout {
            self.last_sync = SystemTime::now();
            self.ops_since_sync = 0;
            return true;
        }
        self.ops_since_sync += 1;
        false
    }
}

/// Provides statistics about the progress and results of batch processing
/// sessions.
///
/// The `Stats` struct contains counts for successful, failed, and skipped
/// tasks, as well as timing and rate information for both the current and
/// previous sessions.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Stats {
    /// Total number of tasks in the session.
    pub total: usize,

    /// Number of tasks successful in the current session.
    pub session_successful: usize,

    /// Number of tasks failed in the current session.
    pub session_failed: usize,

    /// Number of tasks skipped in the current session.
    pub session_skipped: usize,

    /// Number of tasks successful in the previous session.
    pub previous_session_successful: usize,

    /// Number of tasks failed in the previous session.
    pub previous_session_failed: usize,

    /// Elapsed time since the session started.
    pub elapsed: Duration,

    /// Completion rate (tasks per second) for the current session.
    pub rate: f64,
}

impl Display for Stats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Total: {}, Successful: {}, Failed: {}, Skipped: {}, Elapsed: {:.2?}, Rate: {:.2} tasks/sec. In previous session: successful: {}, failed: {}",
            self.total,
            self.session_successful,
            self.session_failed,
            self.session_skipped,
            self.elapsed,
            self.rate,
            self.previous_session_successful,
            self.previous_session_failed,
        )
    }
}

/// Maintains the successful and failed tasks for a session.
///
/// # Type Parameters
/// - `T`: The type representing a task. Must implement `Serialize`,
///   `Deserialize`, `Eq`, and `Hash`.
/// - `S`: The type representing a successful result for a task.
/// - `F`: The type representing a failure result for a task.
///
/// Used internally to track which tasks have been successful or failed, and to
/// load/save session state from files.
struct SessionState<T, S, F>
where
    T: Serialize + for<'de> Deserialize<'de> + Eq + std::hash::Hash,
    S: Serialize + for<'de> Deserialize<'de> + Eq + std::hash::Hash,
    F: Serialize + for<'de> Deserialize<'de> + Eq + std::hash::Hash,
{
    successful: HashMap<T, S>,
    failed: HashMap<T, F>,
}

impl<T, S, F> SessionState<T, S, F>
where
    T: Serialize + for<'de> Deserialize<'de> + Eq + std::hash::Hash,
    S: Serialize + for<'de> Deserialize<'de> + Eq + std::hash::Hash,
    F: Serialize + for<'de> Deserialize<'de> + Eq + std::hash::Hash,
{
    /// Creates a new, empty `SessionState` with no successful or failed tasks.
    pub fn new() -> Self {
        Self {
            successful: HashMap::new(),
            failed: HashMap::new(),
        }
    }

    /// Loads a `SessionState` from the given successful and failed files.
    ///
    /// # Arguments
    /// * `successful_path` - Path to the file containing successful tasks (one
    ///   per line, JSON-encoded)
    /// * `failed_path` - Path to the file containing failed tasks (one per
    ///   line, JSON-encoded)
    ///
    /// # Errors
    /// Returns an error if either file cannot be read or parsed.
    pub fn load(
        successful_path: &Path,
        failed_path: &Path,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        // Load successful URLs
        let successful = if successful_path.exists() {
            let file = File::open(successful_path)?;
            let reader = BufReader::new(file);
            let mut m = HashMap::new();
            for line in reader.lines() {
                let line = line?;
                let (task, s) = from_str(&line)?;
                m.insert(task, s);
            }
            m
        } else {
            HashMap::new()
        };

        // Load failed URLs
        let failed = if failed_path.exists() {
            let file = File::open(failed_path)?;
            let reader = BufReader::new(file);
            reader
                .lines()
                .map_while(Result::ok)
                .map(|s| from_str(&s).unwrap())
                .collect()
        } else {
            HashMap::new()
        };

        Ok(Self { successful, failed })
    }

    /// Returns `true` if the given task is marked as successful in this
    /// session.
    pub fn is_successful(&self, task: &T) -> bool {
        self.successful.contains_key(task)
    }

    /// Returns `true` if the given task is marked as failed in this session.
    pub fn is_failed(&self, task: &T) -> bool {
        self.failed.contains_key(task)
    }

    /// Marks the given task as successful and stores its success value.
    ///
    /// If the task was already marked as successful, this is a no-op.
    pub fn mark_successful(
        &mut self,
        task: T,
        success: S,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.successful.entry(task).or_insert(success);
        Ok(())
    }

    /// Marks the given task as failed and stores its failure value.
    ///
    /// If the task was already marked as failed, this is a no-op.
    pub fn mark_failed(&mut self, task: T, failure: F) -> Result<(), Box<dyn std::error::Error>> {
        self.failed.entry(task).or_insert(failure);
        Ok(())
    }
}

/// Internal struct for managing session state and file persistence for batch
/// processing tasks.
///
/// This struct is not intended to be used directly. Use [`Foothold`] for a
/// thread-safe, user-facing API.
struct FootholdInner<T, S, F>
where
    T: Serialize + for<'de> Deserialize<'de> + Eq + std::hash::Hash,
    S: Serialize + for<'de> Deserialize<'de> + Eq + std::hash::Hash,
    F: Serialize + for<'de> Deserialize<'de> + Eq + std::hash::Hash,
{
    current: SessionState<T, S, F>,
    previous: SessionState<T, S, F>,
    successful_file: Mutex<BufWriter<File>>,
    failed_file: Mutex<BufWriter<File>>,
    skipped: AtomicUsize,
    total: usize,
    start_time: SystemTime,
    sync_config: SyncConfig,
}

impl<T, S, F> FootholdInner<T, S, F>
where
    T: Serialize + for<'de> Deserialize<'de> + Eq + std::hash::Hash,
    S: Serialize + for<'de> Deserialize<'de> + Eq + std::hash::Hash,
    F: Serialize + for<'de> Deserialize<'de> + Eq + std::hash::Hash,
{
    /// Creates a new `FootholdInner`, loading previous session state and
    /// opening files for writing.
    ///
    /// # Arguments
    /// * `successful_path` - Path to the file for successful tasks
    /// * `failed_path` - Path to the file for failed tasks
    /// * `total` - Total number of tasks to process
    ///
    /// # Errors
    /// Returns an error if files cannot be opened or previous state cannot be
    /// loaded.
    pub fn new(
        successful_path: &Path,
        failed_path: &Path,
        total: usize,
        sync_config: SyncConfig,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        // Load previous session state
        let previous = SessionState::load(successful_path, failed_path)?;

        // Open files for appending
        let successful_file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(successful_path)?;

        let failed_file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(failed_path)?;

        Ok(Self {
            previous,
            total,
            sync_config,
            current: SessionState::new(),
            skipped: AtomicUsize::new(0),
            start_time: SystemTime::now(),
            successful_file: Mutex::new(BufWriter::new(successful_file)),
            failed_file: Mutex::new(BufWriter::new(failed_file)),
        })
    }

    /// Returns `true` if the given task is successful in either the current or
    /// previous session.
    pub fn is_successful(&self, task: &T) -> bool {
        self.current.is_successful(task) || self.previous.is_successful(task)
    }

    /// Returns `true` if the given task failed in either the current or
    /// previous session.
    pub fn is_failed(&self, task: &T) -> bool {
        self.current.is_failed(task) || self.previous.is_failed(task)
    }

    /// Marks a task as successful, persists it to file, and updates statistics.
    ///
    /// If the task was already successful in a previous session, it is counted
    /// as skipped.
    pub fn mark_successful(
        &mut self,
        task: T,
        success: S,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let ccomp = self.current.is_successful(&task);
        let pcomp = self.previous.is_successful(&task);
        if !ccomp && !pcomp {
            let mut writer = self.successful_file.lock().unwrap();
            writeln!(writer, "{}", serde_json::to_string(&(&task, &success))?)?;
            self.current.mark_successful(task, success)?;
        } else if pcomp {
            self.mark_skipped(task)?;
        }
        if self.sync_config.should_sync() {
            self.sync_files()?;
        }
        Ok(())
    }

    /// Marks a task as failed, persists it to file, and updates statistics.
    pub fn mark_failed(&mut self, task: T, failure: F) -> Result<(), Box<dyn std::error::Error>> {
        // Write to file
        {
            let mut writer = self.failed_file.lock().unwrap();
            writeln!(writer, "{}", serde_json::to_string(&(&task, &failure))?)?;
            writer.flush()?;
            self.current.mark_failed(task, failure)?;
        }
        if self.sync_config.should_sync() {
            self.sync_files()?;
        }

        Ok(())
    }

    fn sync_files(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        self.successful_file.lock().unwrap().flush()?;
        self.failed_file.lock().unwrap().flush()?;
        Ok(())
    }

    /// Marks a task as skipped, as it was successful during previous session.
    pub fn mark_skipped(&mut self, _task: T) -> Result<(), Box<dyn std::error::Error>> {
        self.skipped.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    /// Returns statistics for the current and previous session.
    pub fn stats(&self) -> Stats {
        Stats {
            session_successful: self.current.successful.len(),
            session_failed: self.current.failed.len(),
            session_skipped: self.skipped.load(Ordering::SeqCst),
            previous_session_successful: self.previous.successful.len(),
            previous_session_failed: self.previous.failed.len(),
            total: self.total,
            elapsed: self.start_time.elapsed().unwrap(),
            rate: self.current.successful.len() as f64
                / self.start_time.elapsed().unwrap().as_secs_f64(),
        }
    }
}

/// Main entry point for managing session state and file persistence for batch
/// processing tasks.
///
/// # Type Parameters
/// - `T`: The type representing a task. Must implement `Serialize`,
///   `Deserialize`, `Eq`, and `Hash`.
/// - `S`: The type representing a successful result for a task.
/// - `F`: The type representing a failure result for a task.
///
/// This struct provides a thread-safe API for marking tasks as successful or
/// failed, and for retrieving session statistics.
pub struct Foothold<T, S, F>
where
    T: Serialize + for<'de> Deserialize<'de> + Eq + std::hash::Hash,
    S: Serialize + for<'de> Deserialize<'de> + Eq + std::hash::Hash,
    F: Serialize + for<'de> Deserialize<'de> + Eq + std::hash::Hash,
{
    inner: Arc<Mutex<FootholdInner<T, S, F>>>,
}

impl<T, S, F> Foothold<T, S, F>
where
    T: Serialize + for<'de> Deserialize<'de> + Eq + std::hash::Hash,
    S: Serialize + for<'de> Deserialize<'de> + Eq + std::hash::Hash,
    F: Serialize + for<'de> Deserialize<'de> + Eq + std::hash::Hash,
{
    /// Creates a new `Foothold` instance, loading previous session state and
    /// opening files for writing.
    ///
    /// # Arguments
    /// * `successful_path` - Path to the file for successful tasks
    /// * `failed_path` - Path to the file for failed tasks
    /// * `total` - Total number of tasks to process
    ///
    /// # Errors
    /// Returns an error if files cannot be opened or previous state cannot be
    /// loaded.
    pub fn new(
        successful_path: &Path,
        failed_path: &Path,
        total: usize,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let sync_config = SyncConfig::create(None, Some(1));
        Self::with_sync(successful_path, failed_path, total, sync_config)
    }

    /// Creates a new `Foothold` instance with a custom sync configuration.
    ///
    /// # Arguments
    /// * `successful_path` - Path to the file for successful tasks
    /// * `failed_path` - Path to the file for failed tasks
    /// * `total` - Total number of tasks to process
    /// * `sync_config` - Configuration for synchronizing session state to disk
    ///
    /// # Errors
    /// Returns an error if files cannot be opened or previous state cannot be
    /// loaded.
    pub fn with_sync(
        successful_path: &Path,
        failed_path: &Path,
        total: usize,
        sync_config: SyncConfig,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let inner = FootholdInner::new(successful_path, failed_path, total, sync_config)?;
        Ok(Self {
            inner: Arc::new(Mutex::new(inner)),
        })
    }
    /// Returns `true` if the given task is successful in either the current or
    /// previous session.
    pub fn is_successful(&self, task: &T) -> bool {
        self.inner.lock().unwrap().is_successful(task)
    }

    /// Returns `true` if the given task is marked as failed in this session.
    pub fn is_failed(&self, task: &T) -> bool {
        self.inner.lock().unwrap().is_failed(task)
    }

    /// Marks a task as successful, persists it to file, and updates statistics.
    ///
    /// If the task was already successful in a previous session, it is counted
    /// as skipped.
    pub fn mark_successful(
        &mut self,
        task: T,
        success: S,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.inner.lock().unwrap().mark_successful(task, success)
    }

    /// Marks a task as failed, persists it to file, and updates statistics.
    pub fn mark_failed(&mut self, task: T, failure: F) -> Result<(), Box<dyn std::error::Error>> {
        self.inner.lock().unwrap().mark_failed(task, failure)
    }

    /// Marks a task as skipped, as it was successful during previous session.
    pub fn mark_skipped(&mut self, task: T) -> Result<(), Box<dyn std::error::Error>> {
        self.inner.lock().unwrap().mark_skipped(task)
    }

    /// Returns statistics for the current and previous session.
    pub fn stats(&self) -> Stats {
        self.inner.lock().unwrap().stats()
    }
}

#[cfg(test)]
mod tests {
    use std::fs::File;
    use std::io::Write;

    use serde::Deserialize;
    use serde::Serialize;

    use super::*;

    fn run_tasks(tasks: &[String], foothold: &mut Foothold<String, (), ()>, fake_failure: bool) {
        for (index, task) in tasks.iter().enumerate() {
            let task = task.to_string();
            if foothold.is_successful(&task) {
                println!("skipping {task}");
                foothold.mark_skipped(task).unwrap();
                continue;
            } else if fake_failure && foothold.is_failed(&task) {
                // If previously failed, skip (or retry)
                println!("skipping {task}");
                foothold.mark_skipped(task).unwrap();
                continue;
            }

            if fake_failure && index % 2 == 1 {
                println!("failing {task}");
                foothold.mark_failed(task, ()).unwrap();
            } else {
                println!("completing {task}");
                foothold.mark_successful(task, ()).unwrap();
            }
        }
    }

    #[test]
    fn test_simple_str() {
        let successful_file = tempfile::NamedTempFile::new().unwrap();
        let failed_file = tempfile::NamedTempFile::new().unwrap();
        let successful = successful_file.path();
        let failed = failed_file.path();
        let mut tasks = vec![
            "task1".to_string(),
            "task2".to_string(),
            "task3".to_string(),
        ];

        {
            let mut foothold =
                Foothold::<String, (), ()>::new(successful, failed, tasks.len()).unwrap();
            run_tasks(&tasks, &mut foothold, true);
            let stats = foothold.stats();
            println!("Stats after first run: {}", stats);
            assert_eq!(stats.session_successful, 2);
            assert_eq!(stats.session_failed, 1);
            assert_eq!(stats.session_skipped, 0);
        }

        // More tasks get added
        tasks.push("task4".to_string());

        {
            let mut foothold =
                Foothold::<String, (), ()>::new(successful, failed, tasks.len()).unwrap();
            run_tasks(&tasks, &mut foothold, false);
            let stats = foothold.stats();
            println!("Stats after resume: {}", stats);
            assert_eq!(stats.session_successful, 2);
            assert_eq!(stats.session_failed, 0);
            assert_eq!(stats.session_skipped, 2);
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
    struct Task(String);
    #[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
    struct Success(String);
    #[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
    struct Failure(String);

    enum Res {
        Success(Success),
        Failure(Failure),
        Noop(Success),
    }

    #[test]
    fn test_mark_successful_and_stats() {
        let successful_file = tempfile::NamedTempFile::new().unwrap();
        let failed_file = tempfile::NamedTempFile::new().unwrap();
        let successful = successful_file.path();
        let failed = failed_file.path();
        let mut foothold = Foothold::<Task, Success, Failure>::new(successful, failed, 2).unwrap();
        let t1 = Task("a".into());
        let t2 = Task("b".into());
        foothold
            .mark_successful(t1.clone(), Success("ok".into()))
            .unwrap();
        foothold
            .mark_successful(t2.clone(), Success("ok2".into()))
            .unwrap();
        let stats = foothold.stats();
        assert_eq!(stats.session_successful, 2);
        assert_eq!(stats.session_failed, 0);
        assert_eq!(stats.session_skipped, 0);
    }

    #[test]
    fn test_mark_failed_and_stats() {
        let successful_file = tempfile::NamedTempFile::new().unwrap();
        let failed_file = tempfile::NamedTempFile::new().unwrap();
        let successful = successful_file.path();
        let failed = failed_file.path();
        let mut foothold = Foothold::<Task, Success, Failure>::new(successful, failed, 1).unwrap();
        let t1 = Task("fail".into());
        foothold
            .mark_failed(t1.clone(), Failure("err".into()))
            .unwrap();
        let stats = foothold.stats();
        assert_eq!(stats.session_successful, 0);
        assert_eq!(stats.session_failed, 1);
    }

    #[test]
    fn test_is_successful_and_skipped() {
        let successful_file = tempfile::NamedTempFile::new().unwrap();
        let failed_file = tempfile::NamedTempFile::new().unwrap();
        let successful = successful_file.path();
        let failed = failed_file.path();
        // Write a successful task to simulate previous session (as a tuple)
        {
            let mut file = File::create(successful).unwrap();
            // Write a valid JSON tuple: (Task, Success)
            writeln!(file, "[\"already_done\",\"ok_prev\"]").unwrap();
        }
        let mut foothold = Foothold::<Task, Success, Failure>::new(successful, failed, 1).unwrap();
        let t1 = Task("already_done".into());
        assert!(foothold.is_successful(&t1));
        foothold
            .mark_successful(t1.clone(), Success("ok".into()))
            .unwrap();
        let stats = foothold.stats();
        assert_eq!(stats.session_skipped, 1);
    }

    fn session(
        foothold: &mut Foothold<Task, Success, Failure>,
        tasks: &[(Task, Res)],
        honor_noop: bool,
    ) {
        for (task, result) in tasks {
            match result {
                Res::Success(s) => {
                    foothold
                        .mark_successful(task.to_owned(), s.to_owned())
                        .unwrap();
                }
                Res::Failure(f) => {
                    foothold.mark_failed(task.to_owned(), f.to_owned()).unwrap();
                }
                Res::Noop(s) => {
                    if !honor_noop {
                        foothold
                            .mark_successful(task.to_owned(), s.to_owned())
                            .unwrap();
                    }
                }
            }
        }
    }

    #[test]
    fn test_resume() {
        let successful_file = tempfile::NamedTempFile::new().unwrap();
        let failed_file = tempfile::NamedTempFile::new().unwrap();
        let successful = successful_file.path();
        let failed = failed_file.path();

        let tasks = vec![
            (Task("task1".into()), Res::Success(Success("ok1".into()))),
            (Task("task2".into()), Res::Failure(Failure("err2".into()))),
            (Task("task3".into()), Res::Success(Success("ok3".into()))),
            (Task("task4".into()), Res::Noop(Success("ok4".into()))),
            (Task("task5".into()), Res::Noop(Success("ok5".into()))),
            (Task("task6".into()), Res::Success(Success("ok6".into()))),
        ];
        // session 1
        {
            let mut foothold =
                Foothold::<Task, Success, Failure>::new(successful, failed, tasks.len()).unwrap();

            let stats = foothold.stats();
            assert_eq!(
                stats,
                Stats {
                    total: tasks.len(),
                    elapsed: stats.elapsed,
                    ..Default::default()
                }
            );
            session(&mut foothold, &tasks, true);

            let stats = foothold.stats();
            assert_eq!(stats.total, tasks.len());
            assert_eq!(stats.previous_session_successful, 0);
            assert_eq!(stats.previous_session_failed, 0);
            assert_eq!(stats.session_successful, 3);
            assert_eq!(stats.session_failed, 1);
            assert_eq!(stats.session_skipped, 0);
            assert_ne!(stats.elapsed.as_nanos(), 0);
            assert_ne!(stats.rate, 0.);
        }
        // session 2 (resume)
        {
            let mut foothold =
                Foothold::<Task, Success, Failure>::new(successful, failed, tasks.len()).unwrap();
            let stats = foothold.stats();
            assert_eq!(stats.total, tasks.len());
            assert_eq!(stats.previous_session_successful, 3);
            assert_eq!(stats.previous_session_failed, 1);
            assert_eq!(stats.session_successful, 0);
            assert_eq!(stats.session_failed, 0);
            assert_eq!(stats.session_skipped, 0);
            assert_ne!(stats.elapsed.as_nanos(), 0);
            assert_eq!(stats.rate, 0.);

            session(&mut foothold, &tasks, false);

            let stats = foothold.stats();
            assert_eq!(stats.total, tasks.len());
            assert_eq!(stats.previous_session_successful, 3);
            assert_eq!(stats.previous_session_failed, 1);
            assert_eq!(stats.session_successful, 2);
            assert_eq!(stats.session_failed, 1);
            assert_eq!(stats.session_skipped, 3);
            assert_ne!(stats.elapsed.as_nanos(), 0);
            assert_ne!(stats.rate, 0.);
        }
    }

    #[test]
    fn test_syncconfig_create_defaults() {
        let config = SyncConfig::create(None, None);
        // Should default to operation_count = Some(1)
        assert_eq!(config.operation_count, Some(1));
        assert_eq!(config.interval, None);
    }

    #[test]
    fn test_syncconfig_create_zero_count() {
        let config = SyncConfig::create(None, Some(0));
        // Should default to operation_count = Some(1)
        assert_eq!(config.operation_count, Some(1));
    }

    #[test]
    fn test_syncconfig_should_sync_by_count() {
        let mut config = SyncConfig::create(None, Some(2));
        // First call: ops_since_sync = 0, returns false
        assert!(!config.should_sync());
        // Second call: ops_since_sync = 1, returns false
        assert!(!config.should_sync());
        // Third call: ops_since_sync = 2, returns true
        assert!(config.should_sync());
        // After sync, ops_since_sync should reset
        assert_eq!(config.ops_since_sync, 0);
    }

    #[test]
    fn test_syncconfig_should_sync_by_interval() {
        let mut config = SyncConfig::create(Some(1), None);
        // Should not sync immediately
        assert!(!config.should_sync());
        // Simulate time passing
        config.last_sync = SystemTime::now() - Duration::from_secs(2);
        // Should sync now
        assert!(config.should_sync());
        // After sync, ops_since_sync should reset
        assert_eq!(config.ops_since_sync, 0);
    }
}
