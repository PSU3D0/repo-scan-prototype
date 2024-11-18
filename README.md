# Git History Analyzer

A quick prototype tool for analyzing git commit histories across repositories, with a focus on lines of code (LOC) statistics. This is a scratch repository intended for experimentation and will not be actively maintained.

## Features

- Analyze git repositories locally or via GitHub API
- Track lines of code changes over time
- Break down statistics by programming language
- Generate CSV and JSON reports
- Merge multiple repository histories
- Support for filtering by author identity patterns

## Setup

1. Create and activate a Python virtual environment:
```bash
python -m venv venv
source venv/bin/activate  # On Windows: venv\Scripts\activate
```

2. Install Python dependencies:
```bash
pip install -r requirements.txt
```

3. Build and install the Rust components:
```bash
maturin develop
```

## Usage

### Analyzing GitHub Repositories

To analyze your GitHub repositories:

```bash
python repo_scan.py --token YOUR_GITHUB_TOKEN --output stats
```

The GitHub token needs `repo` scope access to read private repositories.

### Analyzing Local Repositories

To analyze a local git repository:

```bash
python repo_scan.py --token YOUR_GITHUB_TOKEN --path /path/to/repo
```

### Merging Multiple Histories

If you have multiple LOC history files, you can merge them:

```bash
python merge_loc_histories.py stats/loc_history_1.json stats/loc_history_2.json
```

## Output

The tool generates:
- `loc_history.json`: Detailed statistics in JSON format
- `loc_history.csv`: Monthly statistics in CSV format
- Console output with summary statistics

## Future Development

While this repository serves as a prototype, there are plans to develop a more robust and well-documented tool for git statistics analysis. The future tool will feature:

- Better documentation and test coverage
- More flexible configuration options
- Additional analysis metrics
- Improved performance
- Better handling of author identity matching
- Enhanced reporting capabilities

Stay tuned for updates on the new project!

## License

MIT License
