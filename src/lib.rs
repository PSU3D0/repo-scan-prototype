use std::collections::{HashMap, HashSet, BTreeMap};
use std::path::Path;
use std::sync::Arc;

use chrono::{DateTime, TimeZone, Utc, Datelike};
use git2::{Repository, Commit, Oid};
use parking_lot::Mutex;
use path_slash::PathExt;
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use rayon::prelude::*;
use regex::Regex;
use thiserror::Error;
use indicatif::{ProgressBar, ProgressStyle};

#[derive(Error, Debug)]
pub enum AnalyzerError {
    #[error("Git error: {0}")]
    GitError(#[from] git2::Error),
    #[error("Invalid regex pattern: {0}")]
    RegexError(#[from] regex::Error),
}

#[derive(Debug, Default, Clone)]
struct FileStats {
    lines: i32,
    files: i32,
    additions: i32,
    deletions: i32,
    modifications: i32,
    repos: i32,
}

type MonthlyStats = HashMap<String, HashMap<String, FileStats>>;

#[derive(Debug)]
struct CommitData {
    timestamp: i64,
    message: String,
    author: String,
    stats: HashMap<String, FileStats>,
}

const TEXT_EXTENSIONS: &[&str] = &[
    ".txt", ".md", ".rs", ".py", ".js", ".ts", ".jsx", ".tsx",
    ".html", ".css", ".scss", ".json", ".yaml", ".yml", ".toml",
    ".c", ".cpp", ".h", ".hpp", ".java", ".go", ".rb", ".php"
];

#[pyfunction]
fn analyze_git_commits(
    repo_path: String,
    patterns: Vec<String>,
    show_progress: Option<bool>,
    py: Python<'_>,
) -> PyResult<BTreeMap<String, HashMap<String, PyObject>>> {
    let compiled_patterns = patterns
        .into_iter()
        .map(|p| Regex::new(&p))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| PyValueError::new_err(e.to_string()))?;

    py.allow_threads(|| {
        let commits = analyze_commits_internal(&repo_path, &compiled_patterns, show_progress.unwrap_or(false))
            .map_err(|e| PyValueError::new_err(e.to_string()))?;
        
        // Convert to Python-friendly format
        let mut result = BTreeMap::new();
        
        for (commit_id, commit_data) in commits {
            let mut commit_dict = HashMap::new();
            
            // Convert timestamp
            commit_dict.insert("timestamp".to_string(), 
                Python::with_gil(|py| commit_data.timestamp.into_py(py)));
            
            // Add message and author
            commit_dict.insert("message".to_string(),
                Python::with_gil(|py| commit_data.message.into_py(py)));
            commit_dict.insert("author".to_string(),
                Python::with_gil(|py| commit_data.author.into_py(py)));
            
            // Convert file stats
            let stats_dict: HashMap<String, HashMap<String, i32>> = commit_data.stats
                .into_iter()
                .map(|(ext, stats)| {
                    (ext, HashMap::from([
                        ("lines".to_string(), stats.lines),
                        ("files".to_string(), stats.files),
                        ("additions".to_string(), stats.additions),
                        ("deletions".to_string(), stats.deletions),
                        ("modifications".to_string(), stats.modifications),
                    ]))
                })
                .collect();
            
            commit_dict.insert("stats".to_string(),
                Python::with_gil(|py| stats_dict.into_py(py)));
            
            result.insert(commit_id, commit_dict);
        }
        
        Ok(result)
    })
}

#[pyfunction]
fn analyze_git_repo(
    repo_path: String,
    patterns: Vec<String>,
    show_progress: Option<bool>,
    py: Python<'_>,
) -> PyResult<HashMap<String, HashMap<String, HashMap<String, i32>>>> {
    let compiled_patterns = patterns
        .into_iter()
        .map(|p| Regex::new(&p))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| PyValueError::new_err(e.to_string()))?;

    py.allow_threads(|| {
        analyze_repo_internal(&repo_path, &compiled_patterns, show_progress.unwrap_or(false))
            .map_err(|e| PyValueError::new_err(e.to_string()))
    })
}
fn analyze_repo_internal(
    repo_path: &str,
    patterns: &[Regex],
    show_progress: bool,
) -> Result<HashMap<String, HashMap<String, HashMap<String, i32>>>, AnalyzerError> {
    let repo = Repository::open(repo_path)?;
    let unique_files = Arc::new(Mutex::new(HashSet::new()));
    let monthly_stats = Arc::new(Mutex::new(MonthlyStats::new()));
    
    let mut revwalk = repo.revwalk()?;
    revwalk.push_head()?;
    
    let commits: Vec<Oid> = revwalk.collect::<Result<Vec<_>, _>>()?;
    
    let progress_bar = if show_progress {
        let pb = ProgressBar::new(commits.len() as u64);
        pb.set_style(ProgressStyle::default_bar()
            .template("[{elapsed_precise}] {bar:40.cyan/blue} {pos}/{len} commits")
            .expect("Invalid progress bar template"));
        Some(pb)
    } else {
        None
    };

    commits.iter().try_for_each(|&oid| -> Result<(), AnalyzerError> {
        if let Some(pb) = &progress_bar {
            pb.inc(1);
        }
        let commit = repo.find_commit(oid)?;
        
        // Check if commit author matches any pattern
        let author = format!("{} <{}>", 
            commit.author().name().unwrap_or(""),
            commit.author().email().unwrap_or(""));
        
        if !patterns.is_empty() && !patterns.iter().any(|p| p.is_match(&author)) {
            return Ok(());
        }
        
        process_commit(&repo, &commit, &unique_files, &monthly_stats)?;
        
        Ok(())
    })?;
    
    // Convert internal representation to Python-friendly format
    let result = convert_to_python_format(&monthly_stats.lock());
    Ok(result)
}
    
fn process_commit(
    repo: &Repository,
    commit: &Commit,
    unique_files: &Arc<Mutex<HashSet<String>>>,
    monthly_stats: &Arc<Mutex<MonthlyStats>>,
) -> Result<(), AnalyzerError> {
    let date: DateTime<Utc> = Utc.timestamp_opt(commit.author().when().seconds(), 0)
        .single()
        .unwrap_or_default();
    let month_key = format!("{}-{:02}", date.year(), date.month());
    
    // Handle both first commit and subsequent commits
    let diff = if let Ok(parent) = commit.parent(0) {
        // Normal case - diff against parent
        repo.diff_tree_to_tree(
            Some(&parent.tree()?),
            Some(&commit.tree()?),
            None,
        )?
    } else {
        // First commit - diff against empty tree
        repo.diff_tree_to_tree(
            None,
            Some(&commit.tree()?),
            None,
        )?
    };
    
    let mut new_files = Vec::new();  // For file additions
    let mut file_changes: HashMap<String, (i32, i32)> = HashMap::new();  // Track per-file changes
    
    diff.foreach(
        &mut |delta, _| {
            if let Some(path) = delta.new_file().path() {
                let path_str = path.to_slash_lossy().into_owned();
                let ext = Path::new(&path_str)
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|e| format!(".{}", e.to_lowercase()))
                    .unwrap_or_default();
                
                if TEXT_EXTENSIONS.contains(&ext.as_str()) {
                    let mut unique = unique_files.lock();
                    if !unique.contains(&path_str) {
                        new_files.push(ext);  // Store just the extension
                        unique.insert(path_str);
                    }
                }
            }
            true
        },
        None,
        None,
        Some(&mut |delta, _hunk, lines| {
            if let Some(path) = delta.new_file().path() {
                let ext = Path::new(path)
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|e| format!(".{}", e.to_lowercase()))
                    .unwrap_or_default();
                
                if TEXT_EXTENSIONS.contains(&ext.as_str()) {
                    let mut additions = 0;
                    let mut deletions = 0;
                    
                    // Count actual line changes
                    match lines.origin() {
                        '+' => additions += 1,
                        '-' => deletions += 1,
                        _ => {}
                    }
                    
                    // Accumulate changes per file extension
                    let entry = file_changes.entry(ext).or_insert((0, 0));
                    entry.0 += additions;
                    entry.1 += deletions;
                }
            }
            true
        }),
    )?;

    // Process both types of changes
    let mut stats = monthly_stats.lock();
    for ext in new_files {
        let file_stats = stats.entry(month_key.clone())
            .or_default()
            .entry(ext)
            .or_default();
        file_stats.files += 1;
    }
    
    for (ext, (additions, deletions)) in file_changes {
        let file_stats = stats.entry(month_key.clone())
            .or_default()
            .entry(ext)
            .or_default();
        file_stats.additions += additions;
        file_stats.deletions += deletions;
        file_stats.lines += additions - deletions;
        file_stats.modifications += 1;  // Count one modification per file, not per hunk
    }
    
    Ok(())
}
    
fn convert_to_python_format(
    monthly_stats: &MonthlyStats,
) -> HashMap<String, HashMap<String, HashMap<String, i32>>> {
        let mut result = HashMap::new();
        
        for (month, exts) in monthly_stats {
            let mut month_data = HashMap::new();
            
            for (ext, stats) in exts {
                let stat_map = HashMap::from([
                    ("lines".to_string(), stats.lines),
                    ("files".to_string(), stats.files),
                    ("additions".to_string(), stats.additions),
                    ("deletions".to_string(), stats.deletions),
                    ("modifications".to_string(), stats.modifications),
                    ("repos".to_string(), stats.repos),
                ]);
                
                month_data.insert(ext.clone(), stat_map);
            }
            
            result.insert(month.clone(), month_data);
        }
        
        result
    }

fn analyze_commits_internal(
    repo_path: &str,
    patterns: &[Regex],
    show_progress: bool,
) -> Result<BTreeMap<String, CommitData>, AnalyzerError> {
    let repo = Repository::open(repo_path)?;
    let mut results = BTreeMap::new();
    
    let mut revwalk = repo.revwalk()?;
    revwalk.push_head()?;
    
    let commits: Vec<Oid> = revwalk.collect::<Result<Vec<_>, _>>()?;
    
    let progress_bar = if show_progress {
        let pb = ProgressBar::new(commits.len() as u64);
        pb.set_style(ProgressStyle::default_bar()
            .template("[{elapsed_precise}] {bar:40.cyan/blue} {pos}/{len} commits")
            .expect("Invalid progress bar template"));
        Some(pb)
    } else {
        None
    };

    for oid in commits {
        if let Some(pb) = &progress_bar {
            pb.inc(1);
        }
        let commit = repo.find_commit(oid)?;
        
        // Check if commit author matches any pattern
        let author = format!("{} <{}>", 
            commit.author().name().unwrap_or(""),
            commit.author().email().unwrap_or(""));
        
        if !patterns.is_empty() && !patterns.iter().any(|p| p.is_match(&author)) {
            continue;
        }
        
        let diff = if let Ok(parent) = commit.parent(0) {
            repo.diff_tree_to_tree(
                Some(&parent.tree()?),
                Some(&commit.tree()?),
                None,
            )?
        } else {
            repo.diff_tree_to_tree(
                None,
                Some(&commit.tree()?),
                None,
            )?
        };
        
        let mut file_changes: HashMap<String, (i32, i32)> = HashMap::new();
        let mut new_files: HashSet<String> = HashSet::new();
        
        // Collect file changes
        diff.foreach(
            &mut |delta, _| {
                if let Some(path) = delta.new_file().path() {
                    let ext = Path::new(path)
                        .extension()
                        .and_then(|e| e.to_str())
                        .map(|e| format!(".{}", e.to_lowercase()))
                        .unwrap_or_default();
                    
                    if TEXT_EXTENSIONS.contains(&ext.as_str()) {
                        new_files.insert(ext);
                    }
                }
                true
            },
            None,
            None,
            Some(&mut |delta, _hunk, lines| {
                if let Some(path) = delta.new_file().path() {
                    let ext = Path::new(path)
                        .extension()
                        .and_then(|e| e.to_str())
                        .map(|e| format!(".{}", e.to_lowercase()))
                        .unwrap_or_default();
                    
                    if TEXT_EXTENSIONS.contains(&ext.as_str()) {
                        let entry = file_changes.entry(ext).or_insert((0, 0));
                        match lines.origin() {
                            '+' => entry.0 += 1,
                            '-' => entry.1 += 1,
                            _ => {}
                        }
                    }
                }
                true
            }),
        )?;
        
        // Aggregate stats per extension
        let mut stats = HashMap::new();
        
        for ext in new_files {
            let file_stats: &mut FileStats = stats.entry(ext).or_default();
            file_stats.files += 1;
        }
        
        for (ext, (additions, deletions)) in file_changes {
            let file_stats = stats.entry(ext).or_default();
            file_stats.additions += additions;
            file_stats.deletions += deletions;
            file_stats.lines += additions - deletions;
            file_stats.modifications += 1;
        }
        
        // Store commit data
        results.insert(
            oid.to_string(),
            CommitData {
                timestamp: commit.author().when().seconds(),
                message: commit.message().unwrap_or("").to_string(),
                author,
                stats,
            }
        );
    }
    
    Ok(results)
}

#[pymodule]
fn repo_scan_rs(_py: Python<'_>, m: &PyModule) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(analyze_git_repo, m)?)?;
    m.add_function(wrap_pyfunction!(analyze_git_commits, m)?)?;
    Ok(())
}
