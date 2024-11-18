from typing import Dict, Any
import json
import argparse
from pathlib import Path

def merge_loc_histories(files: list[Path]) -> Dict[str, Any]:
    """
    Merge multiple LOC history JSON files into a single combined history.
    
    Args:
        files: List of paths to JSON files containing LOC history data
        
    Returns:
        Dict containing the merged LOC history data
    """
    merged: Dict[str, Any] = {}
    
    for file in files:
        with open(file) as f:
            data = json.load(f)
            
        # Merge each month's data
        for month, extensions in data.items():
            if month not in merged:
                merged[month] = {}
                
            # Merge extension stats for this month
            for ext, stats in extensions.items():
                if ext not in merged[month]:
                    merged[month][ext] = stats
                else:
                    # Add up all the numeric fields
                    for field in ['lines', 'files', 'additions', 'deletions', 'modifications', 'repos']:
                        merged[month][ext][field] += stats[field]

    return merged

def main() -> None:
    parser = argparse.ArgumentParser(description='Merge multiple LOC history JSON files')
    parser.add_argument('files', nargs='+', type=Path, help='LOC history JSON files to merge')
    parser.add_argument('-o', '--output', type=Path, default='merged_loc_history.json',
                       help='Output file path (default: merged_loc_history.json)')
    
    args = parser.parse_args()
    
    merged_data = merge_loc_histories(args.files)
    
    # Write merged data to output file
    with open(args.output, 'w') as f:
        json.dump(merged_data, f, indent=2)

if __name__ == '__main__':
    main()