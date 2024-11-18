import asyncio
import argparse
import logging
from repo_scan_rs import analyze_git_repo
import os
import re
from datetime import datetime
from pathlib import Path
import tempfile
import shutil
import json
import csv
from collections import defaultdict
from typing import DefaultDict, Dict, List, Set, Tuple, Optional, Union

from githubkit import GitHub, Response
from githubkit.versions.latest.models import Repository, FullRepository, SimpleUser
from tqdm.asyncio import tqdm as tqdm_asyncio  # Fixed import
from tqdm import tqdm
import coloredlogs
from concurrent.futures import ProcessPoolExecutor, as_completed
import functools

# Set up logging
logger = logging.getLogger(__name__)
coloredlogs.install(
    level='INFO',
    logger=logger,
    fmt='%(asctime)s [%(levelname)s] %(message)s',
    datefmt='%H:%M:%S'
)

class CommitIdentityMatcher:
    def __init__(self, github_user):
        """
        Initialize identity matcher with GitHub user info to build matching patterns.
        """
        self.patterns = self._build_patterns(github_user)
        logger.info(f"Initialized {len(self.patterns)} identity matching patterns")
        logger.debug(f"Patterns: {self.patterns}")
        
    def _build_patterns(self, github_user) -> List[re.Pattern]:
        """
        Build regex patterns based on GitHub user information.
        """
        patterns = []
        
        # Get user information
        name = github_user.name or ""
        login = github_user.login
        email = github_user.email or ""
        
        # Basic email pattern from GitHub
        if email:
            patterns.append(re.compile(re.escape(email), re.IGNORECASE))
            # Add pattern for alternative email domains
            local_part = email.split('@')[0]
            patterns.append(re.compile(f"{re.escape(local_part)}@.*", re.IGNORECASE))
        
        # GitHub-provided noreply email pattern
        patterns.append(re.compile(f"{login}@users.noreply.github.com", re.IGNORECASE))
        
        # Username patterns
        patterns.append(re.compile(rf"\b{re.escape(login)}\b", re.IGNORECASE))
        
        # Name patterns (if provided)
        if name:
            # Full name
            patterns.append(re.compile(rf"\b{re.escape(name)}\b", re.IGNORECASE))
            # First/last name separately
            name_parts = name.split()
            for part in name_parts:
                if len(part) > 2:  # Avoid too short name parts
                    patterns.append(re.compile(rf"\b{re.escape(part)}\b", re.IGNORECASE))
        
        return patterns
    
    def is_users_commit(self, commit_author: str, commit_email: str) -> bool:
        """
        Check if a commit matches any of the user's identity patterns.
        """
        text_to_check = f"{commit_author} <{commit_email}>"
        return any(pattern.search(text_to_check) for pattern in self.patterns)

class FileTypeAnalyzer:
    """Analyzes and categorizes file types"""
    
    # Known text file extensions
    TEXT_EXTENSIONS = {
        # Programming languages
        '.py', '.js', '.java', '.cpp', '.c', '.h', '.hpp', '.cs', '.rb', '.php',
        '.go', '.rs', '.swift', '.kt', '.scala', '.m', '.ts', '.coffee', '.r', '.pl',
        '.lua', '.tsx', '.jsx', '.dart', '.groovy', '.erl', '.el', '.clj', '.jl',
        # Web
        '.html', '.css', '.scss', '.sass', '.less', '.jsx', '.tsx', '.vue',
        # Config/Data
        '.json', '.yml', '.yaml', '.xml', '.toml', '.ini', '.conf', '.cfg',
        # Documentation
        '.md', '.rst', '.txt', '.tex', '.adoc',
        # Shell/Scripts
        '.sh', '.bash', '.zsh', '.fish', '.bat', '.ps1',
        # Other common text formats
        '.sql', '.graphql', '.proto', '.cmake', '.gradle'
    }
    
    @classmethod
    def is_text_file(cls, filename: str) -> bool:
        """Determine if a file is likely a text file based on extension"""
        return any(filename.endswith(ext) for ext in cls.TEXT_EXTENSIONS)
    


def analyze_repo_commits(repo_path: str, identity_patterns: List[re.Pattern]) -> Dict[str, Dict[str, Dict[str, int]]]:
    """Analyze repository commits using Rust implementation"""
    logger.debug(f"Analyzing repository at {repo_path}")
    
    try:
        # Convert regex patterns to strings for Rust
        pattern_strings = [p.pattern for p in identity_patterns]
        
        # Call Rust implementation directly
        return analyze_git_repo(repo_path, pattern_strings)
    except Exception as e:
        logger.error(f"Failed to analyze repo {repo_path}: {str(e)}")
        return {}

class GitHubLocTracker:
    def __init__(self, github_token: str):
        self.github = GitHub(github_token)
        self.temp_dir = tempfile.mkdtemp()
        logger.debug(f"Created temporary directory: {self.temp_dir}")
        
        # Initialize these in setup
        self.user: Optional[SimpleUser] = None
        self.username: str = ""
        self.identity_matcher: Optional[CommitIdentityMatcher] = None

    async def setup(self):
        """Async initialization"""
        response = await self.github.rest.users.async_get_authenticated()
        self.user = response.parsed_data
        self.username = self.user.login
        self.identity_matcher = CommitIdentityMatcher(self.user)
        logger.info(f"Authenticated as user: {self.username}")

    async def get_contributed_repos(self, single_repo: Optional[str] = None) -> List[FullRepository]:
        """
        Get repositories to analyze. Either a single specified repo or all contributed repos.
        
        Args:
            single_repo: Optional repository name in format "owner/repo"
        """
        repos = []
        
        if single_repo:
            try:
                owner, repo_name = single_repo.split('/')
                response = await self.github.rest.repos.async_get(owner=owner, repo=repo_name)
                repos.append(response.parsed_data)
                logger.info(f"Using specified repository: {single_repo}")
            except Exception as e:
                logger.error(f"Failed to get repository {single_repo}: {str(e)}")
                return []
        else:
            logger.info("Fetching all contributed repositories...")
            try:
                # Get all repos for authenticated user
                async for repo in self.github.paginate(
                    self.github.rest.repos.async_list_for_authenticated_user,
                    per_page=100
                ):
                    repos.append(repo)
                
                # Filter repos with commits
                filtered_repos = []
                async def check_commits(repo: FullRepository) -> Optional[FullRepository]:
                    try:
                        commits_response = await self.github.rest.repos.async_list_commits(
                            owner=repo.owner.login,
                            repo=repo.name,
                            author=self.username,
                            per_page=1
                        )
                        if commits_response.parsed_data:
                            logger.debug(f"Found contributions in: {repo.full_name}")
                            return repo
                    except Exception as e:
                        logger.warning(f"Failed to check repo {repo.full_name}: {str(e)}")
                    return None

                check_tasks = [check_commits(repo) for repo in repos]
                results = await tqdm_asyncio.gather(*check_tasks, desc="Checking for contributions")
                repos = [repo for repo in results if repo is not None]
                
            except Exception as e:
                logger.error(f"Failed to fetch repositories: {str(e)}")
                return []
        
        logger.info(f"Found {len(repos)} repositories to analyze")
        return repos

    async def clone_repo(self, repo: Repository) -> Tuple[str, bool]:
        """Clone a repository asynchronously using HTTPS."""
        repo_path = os.path.join(self.temp_dir, repo.name)
        
        try:
            # Get SSH URL - convert HTTPS URL to SSH format
            ssh_url = f"git@github.com:{repo.full_name}.git"
            
            process = await asyncio.create_subprocess_exec(
                'git', 'clone', ssh_url, repo_path,
                stdout=asyncio.subprocess.PIPE,
                stderr=asyncio.subprocess.PIPE
            )
            
            _, stderr = await process.communicate()
            
            if process.returncode != 0:
                logger.error(f"Failed to clone {repo.full_name}: {stderr.decode().strip()}")
                return repo_path, False
                
            return repo_path, True
            
        except Exception as e:
            logger.error(f"Exception while cloning {repo.full_name}: {str(e)}")
            return repo_path, False


    async def process_all_repos(self, single_repo: Optional[str] = None) -> Dict[str, Dict[str, Dict[str, int]]]:
        """Process all repositories and aggregate results."""
        repos = await self.get_contributed_repos(single_repo)
        
        if not repos:
            logger.error("No repositories found to analyze.")
            return {}
        
        logger.info("Cloning repositories...")
        clone_tasks = [self.clone_repo(repo) for repo in repos]
        # Limit concurrency to prevent too many simultaneous git operations
        semaphore = asyncio.Semaphore(5)
        async def sem_clone(repo_task):
            async with semaphore:
                return await repo_task
        results = await tqdm_asyncio.gather(*(sem_clone(task) for task in clone_tasks), desc="Cloning repositories")
        
        repo_paths = [path for path, success in results if success]
        
        if len(repo_paths) == 0:
            logger.error("No repositories were successfully cloned")
            return {}
            
        logger.info(f"Successfully cloned {len(repo_paths)} repositories")

        futures = []
        
        with tqdm(total=len(repo_paths), desc="Analyzing repositories") as pbar:
            with ProcessPoolExecutor(max_workers=min(8, len(repo_paths))) as executor:
                for path in repo_paths:
                    future = executor.submit(analyze_repo_commits, path, self.identity_matcher.patterns)
                    futures.append(future)
            # Gather results with progress
            repo_stats_list = []
            for future in as_completed(futures):
                repo_stats = future.result()
                if repo_stats:
                    repo_stats_list.append(repo_stats)

                pbar.update(1)
        
        # Aggregate all stats
        total_monthly_stats: DefaultDict[str, DefaultDict[str, Dict[str, int]]] = defaultdict(lambda: defaultdict(lambda: {
            'lines': 0, 'files': 0, 'additions': 0,
            'deletions': 0, 'modifications': 0, 'repos': 0
        }))
        
        logger.info("Aggregating statistics...")
        for repo_stats in repo_stats_list:
            for month, stats in repo_stats.items():
                for ext, metrics in stats.items():
                    for metric, value in metrics.items():
                        total_monthly_stats[month][ext][metric] += value
        
        # Convert defaultdict to regular dict
        aggregated_results = {
            month: dict(stats)
            for month, stats in sorted(total_monthly_stats.items())
        }
        
        return aggregated_results

    def cleanup(self):
        """Clean up temporary directory."""
        logger.debug(f"Cleaning up temporary directory: {self.temp_dir}")
        shutil.rmtree(self.temp_dir)
        logger.debug("Cleanup completed")

    def export_results(self, results: Dict[str, Dict[str, Dict[str, int]]], output_dir: str):
        """
        Export results in multiple formats.
        """
        os.makedirs(output_dir, exist_ok=True)
        
        # Export as JSON
        json_path = os.path.join(output_dir, 'loc_history.json')
        with open(json_path, 'w') as f:
            json.dump(results, f, indent=2)
        logger.info(f"Exported JSON results to {json_path}")
        
        # Export as CSV
        csv_path = os.path.join(output_dir, 'loc_history.csv')
        
        # Get all unique language extensions excluding 'total'
        languages = set()
        for month_data in results.values():
            languages.update(k for k in month_data.keys() if k != 'total')
        
        # Define metrics to export
        metrics = ['lines', 'additions', 'deletions', 'modifications', 'repos']
        
        with open(csv_path, 'w', newline='') as f:
            writer = csv.writer(f)
            # Write header
            header = ['Month']
            for lang in sorted(languages):
                for metric in metrics:
                    header.append(f"{lang} {metric}")
            writer.writerow(header)
            # Write data
            for month, stats in sorted(results.items()):
                row = [month]
                for lang in sorted(languages):
                    lang_metrics = stats.get(lang, {})
                    for metric in metrics:
                        row.append(lang_metrics.get(metric, 0))
                writer.writerow(row)
        logger.info(f"Exported CSV results to {csv_path}")

    def format_results(self, results: Dict[str, Dict[str, Dict[str, int]]]) -> str:
        """Format the results for display."""
        if not results:
            return "No data to display"
            
        output = ["Monthly Lines of Code Contribution:", "=" * 35]
        
        # Calculate totals
        total_lines = sum(stats.get("total", {}).get("lines", 0) for stats in results.values())
        total_additions = sum(stats.get("total", {}).get("additions", 0) for stats in results.values())
        total_deletions = sum(stats.get("total", {}).get("deletions", 0) for stats in results.values())
        total_modifications = sum(stats.get("total", {}).get("modifications", 0) for stats in results.values())
        
        if total_lines == 0:
            logger.warning("Total lines of code is zero. Cannot determine the most productive month.")
            max_month = "N/A"
            max_lines = 0
        else:
            max_month, max_stats = max(results.items(), key=lambda x: x[1].get("total", {}).get("lines", 0))
            max_lines = max_stats.get("total", {}).get("lines", 0)
        
        # Get language statistics
        language_stats: DefaultDict[str, Dict[str, Union[int, Set[str]]]] = defaultdict(lambda: {
            'lines': 0, 'files': 0, 'additions': 0,
            'deletions': 0, 'modifications': 0, 'repos': 0
        })
        
        for stats in results.values():
            for lang, metrics in stats.items():
                if lang != "total":
                    language_stats[lang]['lines'] += metrics.get('lines', 0)
                    language_stats[lang]['files'] += metrics.get('files', 0)
                    language_stats[lang]['additions'] += metrics.get('additions', 0)
                    language_stats[lang]['deletions'] += metrics.get('deletions', 0)
                    language_stats[lang]['modifications'] += metrics.get('modifications', 0)
                    language_stats[lang]['repos'] += metrics.get('repos', 0)
        
        # Add summary statistics
        output.extend([
            f"Total Statistics:",
            f"  Lines of code: {total_lines:,}",
            f"  Additions: {total_additions:,}",
            f"  Deletions: {total_deletions:,}",
            f"  File modifications: {total_modifications:,}",
            f"Most productive month: {max_month} ({max_lines:,} lines)",
            f"Months analyzed: {len(results)}",
            "",
            "Language Breakdown:",
            "-" * 50
        ])
        
        # Add language breakdown
        for lang, stats in sorted(language_stats.items(), key=lambda x: x[1]['lines'], reverse=True):
            if lang != "total":
                percentage = (stats['lines'] / total_lines) * 100 if total_lines else 0
                output.extend([
                    f"{lang or 'unknown'}:",
                    f"  Lines: {stats['lines']:,} ({percentage:.1f}%)",
                    f"  Files: {stats['files']:,}",
                    f"  Additions: {stats['additions']:,}",
                    f"  Deletions: {stats['deletions']:,}",
                    f"  Modifications: {stats['modifications']:,}",
                    ""
                ])
        
        output.extend([
            "",
            "Monthly Breakdown:",
            "-" * 20
        ])
        
        # Add monthly breakdown
        for month, stats in sorted(results.items()):
            total = stats.get("total", {})
            percentage = (total.get('lines', 0) / total_lines) * 100 if total_lines else 0
            output.append(f"{month}: {total.get('lines', 0):,} lines ({percentage:.1f}%)")
            
        return "\n".join(output)

async def main():
    parser = argparse.ArgumentParser(description='Analyze GitHub repository contributions')
    parser.add_argument('--path', help='Path to local repository')
    parser.add_argument('--repo', help='Analyze single repository in format owner/repo')
    parser.add_argument('--token', help='GitHub token', required=True)
    parser.add_argument('--output', help='Output directory', default="github_stats")
    args = parser.parse_args()
    
    tracker = GitHubLocTracker(args.token)
    
    try:
        logger.info("Starting GitHub LOC analysis...")
        await tracker.setup()
        
        # Process all repositories with concurrency
        results = await tracker.process_all_repos(args.repo)
        
        if not results:
            logger.error("No data to export or display.")
            return
        
        # Export results
        tracker.export_results(results, args.output)
        
        # Print formatted results
        print("\n" + tracker.format_results(results))
            
    except Exception as e:
        logger.error(f"An error occurred: {str(e)}")
        raise
        
    finally:
        tracker.cleanup()
        logger.info("Analysis completed")

if __name__ == "__main__":
    asyncio.run(main())
