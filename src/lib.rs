use std::collections::{HashMap, HashSet};
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

#[derive(Error, Debug)]
pub enum AnalyzerError {
    #[error("Git error: {0}")]
    GitError(#[from] git2::Error),
    #[error("Invalid regex pattern: {0}")]
    RegexError(#[from] regex::Error),
}

#[derive(Debug, Default)]
struct FileStats {
    lines: i32,
    files: i32,
    additions: i32,
    deletions: i32,
    modifications: i32,
    repos: i32,
}

type MonthlyStats = HashMap<String, HashMap<String, FileStats>>;

const TEXT_EXTENSIONS: &[&str] = &[
    ".txt", ".md", ".rs", ".py", ".js", ".ts", ".jsx", ".tsx",
    ".html", ".css", ".scss", ".json", ".yaml", ".yml", ".toml",
    ".c", ".cpp", ".h", ".hpp", ".java", ".go", ".rb", ".php"
];

#[pyfunction]
fn analyze_git_repo(
    repo_path: String,
    patterns: Vec<String>,
    py: Python<'_>,
) -> PyResult<HashMap<String, HashMap<String, HashMap<String, i32>>>> {
    let compiled_patterns = patterns
        .into_iter()
        .map(|p| Regex::new(&p))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| PyValueError::new_err(e.to_string()))?;

    py.allow_threads(|| {
        analyze_repo_internal(&repo_path, &compiled_patterns)
            .map_err(|e| PyValueError::new_err(e.to_string()))
    })
}
fn analyze_repo_internal(
    repo_path: &str,
    patterns: &[Regex],
) -> Result<HashMap<String, HashMap<String, HashMap<String, i32>>>, AnalyzerError> {
    let repo = Repository::open(repo_path)?;
    let unique_files = Arc::new(Mutex::new(HashSet::new()));
    let monthly_stats = Arc::new(Mutex::new(MonthlyStats::new()));
    
    let mut revwalk = repo.revwalk()?;
    revwalk.push_head()?;
    
    let commits: Vec<Oid> = revwalk.collect::<Result<Vec<_>, _>>()?;
    
    commits.iter().try_for_each(|&oid| -> Result<(), AnalyzerError> {
        let commit = repo.find_commit(oid)?;
        
        // Check if commit author matches any pattern
        let author = format!("{} <{}>", 
            commit.author().name().unwrap_or(""),
            commit.author().email().unwrap_or(""));
        
        if !patterns.iter().any(|p| p.is_match(&author)) {
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
        
        if let Some(parent) = commit.parent(0).ok() {
            let diff = repo.diff_tree_to_tree(
                Some(&parent.tree()?),
                Some(&commit.tree()?),
                None,
            )?;
            
            let mut new_files = Vec::new();  // For file additions
            let mut modifications = Vec::new();  // For content changes
            
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
                Some(&mut |delta, hunk, _lines| {
                    if let Some(path) = delta.new_file().path() {
                        let ext = Path::new(path)
                            .extension()
                            .and_then(|e| e.to_str())
                            .map(|e| format!(".{}", e.to_lowercase()))
                            .unwrap_or_default();
                        
                        if TEXT_EXTENSIONS.contains(&ext.as_str()) {
                            if let Some(hunk) = hunk {
                                modifications.push((ext, (hunk.new_lines(), hunk.old_lines())));
                            }
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
            
            for (ext, (new_lines, old_lines)) in modifications {
                let file_stats = stats.entry(month_key.clone())
                    .or_default()
                    .entry(ext)
                    .or_default();
                file_stats.additions += new_lines as i32;
                file_stats.deletions += old_lines as i32;
                file_stats.lines += new_lines as i32 - old_lines as i32;
                file_stats.modifications += 1;
            }
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

#[pymodule]
fn self_repo_scan(_py: Python<'_>, m: &PyModule) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(analyze_git_repo, m)?)?;
    Ok(())
}
