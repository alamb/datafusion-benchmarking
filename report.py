#!/usr/bin/env python3
"""
DataFusion Benchmarking Report Generator

This script analyzes benchmark results from CSV files and generates comprehensive
performance reports with charts and statistics.

Prerequisites:
    pip install pandas matplotlib seaborn datafusion numpy plotly

Usage:
    # Basic usage - analyze results from 'results/' directory, output to 'docs/'
    ./report.py

    # Specify custom results directory
    ./report.py --results-dir results

Input:
    - CSV files from benchmark runs (typically in results/ directory)
    - Expected CSV format: benchmark_name, query_name, query_type, execution_time,
      run_timestamp, git_revision, git_revision_timestamp, num_cores

Output:
    - docs/index.html: Comprehensive interactive report with analysis

The script uses DataFusion's Python package for SQL-based analysis and Plotly
for generating interactive performance charts with git revision timestamps on the x-axis.
"""

import argparse
import os
import glob
import pandas as pd
from datetime import datetime
from datafusion import SessionContext
import json



def main():
    parser = argparse.ArgumentParser(description="Analyze benchmark results and generate reports")
    parser.add_argument('--results-dir', default='results', help='Directory containing benchmark result CSV files')
    args = parser.parse_args()

    print("DataFusion Benchmark Report Generator")

    # Output directory is fixed to 'docs'
    output_dir = 'docs'

    # Create output directory if it doesn't exist
    if not os.path.exists(output_dir):
        os.makedirs(output_dir)

    print(f"Analyzing results from: {args.results_dir}")
    print(f"Output will be written to: {output_dir}")

    # Find all CSV files in results directory
    csv_files = glob.glob(os.path.join(args.results_dir, "*.csv"))
    if not csv_files:
        print(f"No CSV files found in {args.results_dir}")
        return

    print(f"Found {len(csv_files)} result files")

    # Create DataFusion session context
    ctx = SessionContext()

    # Register the entire results directory as a single table using glob pattern
    results_dir = os.path.abspath(args.results_dir)
    csv_pattern = os.path.join(results_dir, "*.csv")

    # Create external table that reads all CSV files in the directory
    create_table_sql = f"""
    CREATE EXTERNAL TABLE benchmark_results (
        benchmark_name VARCHAR,
        query_name VARCHAR,
        query_type VARCHAR,
        execution_time DOUBLE,
        run_timestamp VARCHAR,
        git_revision VARCHAR,
        git_revision_timestamp VARCHAR,
        num_cores BIGINT
    )
    STORED AS CSV
    LOCATION '{csv_pattern}'
    OPTIONS ('format.has_header' 'true')
    """

    try:
        ctx.sql(create_table_sql)
        print(f"Registered all CSV files from {args.results_dir} as 'benchmark_results' table")

        # Get row count to verify the table was created successfully
        count_result = ctx.sql("SELECT COUNT(*) as row_count FROM benchmark_results")
        row_count = count_result.to_pandas().iloc[0]['row_count']
        print(f"Total rows loaded: {row_count}")

    except Exception as e:
        print(f"Error creating external table: {e}")
        return

    # Generate analysis and charts
    generate_report(ctx, output_dir)

    print(f"Analysis complete! Check {output_dir}/index.html for results")

def generate_report(ctx, output_dir):
    """Generate an HTML report with inlined interactive charts"""
    print("Generating HTML report with inlined charts...")

    # Get overall statistics
    query = """
    SELECT 
        COUNT(DISTINCT git_revision) as total_revisions,
        COUNT(DISTINCT query_name) as total_queries,
        COUNT(*) as total_measurements,
        MIN(git_revision_timestamp) as earliest_revision,
        MAX(git_revision_timestamp) as latest_revision
    FROM benchmark_results 
    WHERE query_type = 'query'
    """

    result = ctx.sql(query)
    overall_stats = result.to_pandas().iloc[0]

    # Generate the charts and get their HTML content for inlining
    chart_htmls = generate_chart_data_html(ctx, output_dir)

    # Generate the HTML report with inlined charts
    report_content = f"""<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>DataFusion ClickBench Performance Analysis</title>
    <script src="https://cdn.plot.ly/plotly-latest.min.js"></script>
    <style>
        body {{
            font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, Oxygen, Ubuntu, Cantarell, sans-serif;
            line-height: 1.6;
            max-width: 1400px;
            margin: 0 auto;
            padding: 20px;
            background-color: #f8f9fa;
        }}
        .container {{
            background-color: white;
            padding: 30px;
            border-radius: 8px;
            box-shadow: 0 2px 10px rgba(0,0,0,0.1);
        }}
        h1 {{
            color: #2c3e50;
            border-bottom: 3px solid #3498db;
            padding-bottom: 10px;
            text-align: center;
        }}
        h2 {{
            color: #34495e;
            margin-top: 30px;
            border-left: 4px solid #3498db;
            padding-left: 15px;
        }}
        h3 {{
            color: #555;
            margin-top: 25px;
        }}
        .chart-section {{
            margin: 40px 0;
            padding: 20px;
            background-color: #f8f9fa;
            border-radius: 8px;
            border: 1px solid #dee2e6;
        }}
        .chart-container {{
            margin: 20px 0;
            border: 1px solid #ddd;
            border-radius: 8px;
            overflow: hidden;
        }}
        .filter-section {{
            margin: 0;
            padding: 20px;
            background-color: #e3f2fd;
            border-radius: 8px;
            border-left: 4px solid #2196f3;
        }}
        .filter-section h3 {{
            margin-top: 0;
            color: #1976d2;
        }}
        .filter-dropdown {{
            padding: 8px 12px;
            border: 1px solid #ddd;
            border-radius: 4px;
            font-size: 14px;
            background-color: white;
            cursor: pointer;
        }}
        .instructions {{
            background-color: #e3f2fd;
            padding: 20px;
            border-radius: 8px;
            border-left: 4px solid #2196f3;
            margin: 20px 0;
        }}
        .instructions h3 {{
            margin-top: 0;
            color: #1976d2;
        }}
        .instructions ul {{
            margin: 10px 0;
        }}
        .instructions li {{
            margin: 5px 0;
        }}
        .info-grid {{
            display: grid;
            grid-template-columns: 1fr 1fr;
            gap: 20px;
            margin: 20px 0;
        }}
        table {{
            width: 100%;
            border-collapse: collapse;
            margin: 20px 0;
            background-color: white;
        }}
        th, td {{
            padding: 12px;
            text-align: left;
            border-bottom: 1px solid #dee2e6;
        }}
        th {{
            background-color: #f8f9fa;
            font-weight: 600;
            color: #495057;
        }}
        tr:hover {{
            background-color: #f8f9fa;
        }}
        .footer {{
            margin-top: 40px;
            padding-top: 20px;
            border-top: 1px solid #dee2e6;
            text-align: center;
            color: #6c757d;
            font-style: italic;
        }}
        .two-column {{
            display: grid;
            grid-template-columns: 1fr 1fr;
            gap: 20px;
            margin: 20px 0;
        }}
        @media (max-width: 768px) {{
            .two-column {{
                grid-template-columns: 1fr;
            }}
            .info-grid {{
                grid-template-columns: 1fr;
            }}
        }}
    </style>
</head>
<body>
    <div class="container">
        <h1>🚀 DataFusion ClickBench Performance Analysis</h1>

        <div class="info-grid">
            <div class="instructions">
                <h3>💡 How to Use the Interactive Charts</h3>
                <ol>
                    <li><strong>Filter by queries:</strong> Click on query names in legends to show/hide them, double click to focus</li>
                    <li><strong>Compare performance:</strong> Hover to see details</li>
                    <li><strong>Reset views:</strong> Double-click on charts to reset zoom level</li>
                </ol>
            </div>

            <div class="filter-section">
                <h3>📅 Time Period Filter</h3>
                <p>Select a time period to focus your analysis:</p>
                <select id="timeFilter" class="filter-dropdown" onchange="updateCharts()">
                    <option value="releases_vs_main" selected>Releases vs Main</option>
                    <option value="last_week">Last 1 Week</option>
                    <option value="last_3_months">Last 3 Months</option>
                    <option value="last_6_months">Last 6 Months</option>
                    <option value="all">All Data</option>
                </select>
                <div style="margin-top: 15px;">
                    <label style="display: flex; align-items: center; gap: 8px; font-size: 14px;">
                        <input type="checkbox" id="showReleaseLines" checked onchange="updateCharts()" style="margin: 0;">
                        Show releases
                    </label>
                </div>
                <div style="margin-top: 10px;">
                    <label style="display: flex; align-items: center; gap: 8px; font-size: 14px;">
                        <input type="checkbox" id="showEventLines" checked onchange="updateCharts()" style="margin: 0;">
                        Show events
                    </label>
                </div>
                <div id="filterDescription" style="margin-top: 10px; font-style: italic; color: #666;">
                    Releases and most recent git revision
                </div>
            </div>
        </div>

        <h2>📊 Interactive Performance Charts</h2>

        <div class="chart-section">
            <h3>Overall Performance</h3>
            <p>
                Interactive chart: average, and median <strong>normalized query execution times</strong> for all queries for each git revision. 
                Query times are normalized using the <a href="https://github.com/ClickHouse/ClickBench?tab=readme-ov-file#results-usage-and-scoreboards" target="_blank">ClickBench definition</a>: 
                for each query, the fastest time across all revisions is used as a baseline, and normalized times are calculated as 
                <code>(10ms + query_time) / (10ms + baseline_time)</code>. 
                This gives values <code> ≥ 1.0</code>, where <code>1.0</code> represents the best performance for that query.</p>
            <div class="chart-container">
                <div id="performance_chart"></div>
            </div>
        </div>

        <div class="chart-section">
            <h3>Individual Query Performance</h3>
            <p>Interactive chart: Individual query performance over time - click legend items to show/hide specific queries.</p>
            <div class="chart-container">
                <div id="per_query_chart"></div>
            </div>
        </div>


        <h2>🗂️ Data </h2>        
        <p>
            <li>
                The analysis covers <strong>{overall_stats['total_revisions']}</strong> different git revisions
            </li>
            <li>
                <strong>Data from:</strong> {overall_stats['earliest_revision']} to {overall_stats['latest_revision']}
                (<a href="https://github.com/alamb/datafusion-benchmarking/tree/main/results">download</a>)
            </li>
        </p>

        <div class="footer">
            <p>
                <strong>Generated on:</strong> {datetime.now().strftime('%Y-%m-%d %H:%M:%S')}
                with code from <a href="https://github.com/alamb/datafusion-benchmarking">alamb/datafusion-benchmarking</a> 
            </p>
        </div>
    </div>

    <script>
        // Store all chart data for filtering
        var chartData = {json.dumps(chart_htmls['chart_data'], sort_keys=True)};
        var releaseData = {json.dumps(chart_htmls['release_data'], sort_keys=True)};
        
        function updateCharts() {{
            const filter = document.getElementById('timeFilter').value;
            const showReleaseLines = document.getElementById('showReleaseLines').checked;
            const showEventLines = document.getElementById('showEventLines').checked;
            const description = document.getElementById('filterDescription');
            
            let filteredPerformanceData, filteredQueryData, descText;
            
            switch(filter) {{
                case 'all':
                    filteredPerformanceData = chartData.performance;
                    filteredQueryData = chartData.queries;
                    descText = 'Showing all available data';
                    break;
                case 'releases_vs_main':
                    filteredPerformanceData = filterReleasesVsMain(chartData.performance);
                    filteredQueryData = filterReleasesVsMain(chartData.queries);
                    descText = 'Showing releases from releases.json and most recent main branch data';
                    break;
                case 'last_week':
                    filteredPerformanceData = filterLastPeriod(chartData.performance, 7);
                    filteredQueryData = filterLastPeriod(chartData.queries, 7);
                    descText = 'Showing data from the last 7 days';
                    break;
                case 'last_3_months':
                    filteredPerformanceData = filterLastPeriod(chartData.performance, 90);
                    filteredQueryData = filterLastPeriod(chartData.queries, 90);
                    descText = 'Showing data from the last 3 months';
                    break;
                case 'last_6_months':
                    filteredPerformanceData = filterLastPeriod(chartData.performance, 180);
                    filteredQueryData = filterLastPeriod(chartData.queries, 180);
                    descText = 'Showing data from the last 6 months';
                    break;
            }}
            
            // If "Show releases" is unchecked, remove the vertical lines and annotations
            if(!showReleaseLines) {{
                filteredPerformanceData = removeReleaseLines(filteredPerformanceData);
                filteredQueryData = removeReleaseLines(filteredQueryData);
            }}
            
            // If "Show events" is unchecked, remove the event lines
            if(!showEventLines) {{
                filteredPerformanceData = removeEventLines(filteredPerformanceData);
                filteredQueryData = removeEventLines(filteredQueryData);
            }}
            
            description.textContent = descText;
            
            // Update performance chart
            Plotly.newPlot('performance_chart', filteredPerformanceData.data, filteredPerformanceData.layout, filteredPerformanceData.config);
            
            // Update per-query chart
            Plotly.newPlot('per_query_chart', filteredQueryData.data, filteredQueryData.layout, filteredQueryData.config);
        }}
        
        function removeReleaseLines(chartObj) {{
            // Create a copy with release lines removed
            const newLayout = {{...chartObj.layout}};
            newLayout.shapes = newLayout.shapes.filter(shape => 
                shape.CUSTOM_ANNOTATION && shape.CUSTOM_ANNOTATION !== "release"
            );
            newLayout.annotations = newLayout.annotations.filter(ann => 
                ann.CUSTOM_ANNOTATION && ann.CUSTOM_ANNOTATION !== "release"
            );
            
            return {{
                data: chartObj.data,
                layout: newLayout,
                config: chartObj.config
            }};
        }}
        
        function removeEventLines(chartObj) {{
            // Create a copy with event lines removed
            const newLayout = {{...chartObj.layout}};
            newLayout.shapes = newLayout.shapes.filter(shape => 
                shape.CUSTOM_ANNOTATION && shape.CUSTOM_ANNOTATION !== "event"
            );
            newLayout.annotations = newLayout.annotations.filter(ann => 
                ann.CUSTOM_ANNOTATION && ann.CUSTOM_ANNOTATION !== "event"
            );
            
            return {{
                data: chartObj.data,
                layout: newLayout,
                config: chartObj.config
            }};
        }}
        
        function filterReleasesVsMain(chartObj) {{
            // Filter to include only releases and the most recent revision
            const releases = releaseData.releases;
            const mostRecentTimestamp = releaseData.mostRecent;
            const mostRecentDate = mostRecentTimestamp ? new Date(mostRecentTimestamp) : null;
            
            const filteredData = chartObj.data.map(trace => {{
                const filteredX = [];
                const filteredY = [];
                const filteredCustomdata = [];
                
                for(let i = 0; i < trace.x.length; i++) {{
                    const timestamp = new Date(trace.x[i]);
                    const revision = trace.customdata ? trace.customdata[i] : '';
                    
                    // Include if it's a release or the most recent data
                    // Use date comparison for most recent timestamp to handle formatting differences
                    const isMostRecent = mostRecentDate && Math.abs(timestamp.getTime() - mostRecentDate.getTime()) < 1000; // Within 1 second
                    
                    if(releases.includes(revision) || isMostRecent) {{
                        filteredX.push(trace.x[i]);
                        filteredY.push(trace.y[i]);
                        if(trace.customdata) filteredCustomdata.push(trace.customdata[i]);
                    }}
                }}
                
                const newTrace = {{...trace}};
                newTrace.x = filteredX;
                newTrace.y = filteredY;
                if(trace.customdata) newTrace.customdata = filteredCustomdata;
                return newTrace;
            }});
            
            return {{
                data: filteredData,
                layout: chartObj.layout,
                config: chartObj.config
            }};
        }}
        
        function filterLastPeriod(chartObj, days) {{
            const cutoffDate = new Date();
            cutoffDate.setDate(cutoffDate.getDate() - days);
            
            // For short periods (week/month), only include the most recent release and recent data
            const mostRecentRelease = releaseData.mostRecentRelease;
            
            const filteredData = chartObj.data.map(trace => {{
                const filteredX = [];
                const filteredY = [];
                const filteredCustomdata = [];
                
                for(let i = 0; i < trace.x.length; i++) {{
                    const timestamp = new Date(trace.x[i]);
                    const revision = trace.customdata ? trace.customdata[i] : '';
                    
                    // For last week view, only include data from actual last 7 days
                    if(days === 7) {{
                        if(timestamp >= cutoffDate) {{
                            filteredX.push(trace.x[i]);
                            filteredY.push(trace.y[i]);
                            if(trace.customdata) filteredCustomdata.push(trace.customdata[i]);
                        }}
                    }} else {{
                        // For other short periods, include recent data OR the most recent release
                        if(timestamp >= cutoffDate || revision === mostRecentRelease) {{
                            filteredX.push(trace.x[i]);
                            filteredY.push(trace.y[i]);
                            if(trace.customdata) filteredCustomdata.push(trace.customdata[i]);
                        }}
                    }}
                }}
                
                const newTrace = {{...trace}};
                newTrace.x = filteredX;
                newTrace.y = filteredY;
                if(trace.customdata) newTrace.customdata = filteredCustomdata;
                return newTrace;
            }});
            
            // Update layout to filter vertical lines and annotations based on time period
            const newLayout = {{...chartObj.layout}};
            if(days === 7) {{ // For last week view, remove all vertical bars
                newLayout.shapes = [];
                newLayout.annotations = [];
            }} else {{
                // For all other periods, filter shapes and annotations based on cutoff date
                newLayout.shapes = chartObj.layout.shapes ? chartObj.layout.shapes.filter(shape => {{
                    // Parse the timestamp from the shape's x0 coordinate
                    const shapeDate = new Date(shape.x0);
                    
                    // For month views, only show most recent release OR releases within time range
                    if(days <= 30) {{
                        return chartObj.layout.annotations && chartObj.layout.annotations.some(ann => 
                            ann.x === shape.x0 && ann.text && ann.text.includes(mostRecentRelease)
                        );
                    }} else {{
                        // For 3 and 6 month views, only show releases within the time range
                        return shapeDate >= cutoffDate;
                    }}
                }}) : [];
                
                newLayout.annotations = chartObj.layout.annotations ? chartObj.layout.annotations.filter(ann => {{
                    // Parse the timestamp from the annotation's x coordinate
                    const annDate = new Date(ann.x);
                    
                    // For month views, only show most recent release
                    if(days <= 30) {{
                        return ann.text && ann.text.includes(mostRecentRelease);
                    }} else {{
                        // For 3 and 6 month views, only show releases within the time range
                        return annDate >= cutoffDate;
                    }}
                }}) : [];
            }}
            
            return {{
                data: filteredData,
                layout: newLayout,
                config: chartObj.config
            }};
        }}
        
        function filterOutReleases(chartObj) {{
            // Filter out release revisions
            const releases = releaseData.releases;
            
            const filteredData = chartObj.data.map(trace => {{
                const filteredX = [];
                const filteredY = [];
                const filteredCustomdata = [];
                
                for(let i = 0; i < trace.x.length; i++) {{
                    const revision = trace.customdata ? trace.customdata[i] : '';
                    
                    // Exclude if it's a release revision
                    if(!releases.includes(revision)) {{
                        filteredX.push(trace.x[i]);
                        filteredY.push(trace.y[i]);
                        if(trace.customdata) filteredCustomdata.push(trace.customdata[i]);
                    }}
                }}
                
                const newTrace = {{...trace}};
                newTrace.x = filteredX;
                newTrace.y = filteredY;
                if(trace.customdata) newTrace.customdata = filteredCustomdata;
                return newTrace;
            }});
            
            return {{
                data: filteredData,
                layout: chartObj.layout,
                config: chartObj.config
            }};
        }}
        
        // Initialize charts with default filter (releases vs main)
        document.addEventListener('DOMContentLoaded', function() {{
            updateCharts();
        }});
    </script>
</body>
</html>"""

    # Save the report as index.html
    report_path = os.path.join(output_dir, "index.html")
    with open(report_path, 'w', encoding='utf-8') as f:
        f.write(report_content)

    print(f"HTML dashboard saved to: {report_path}")

def generate_chart_data_html(ctx, output_dir):
    """Generate chart HTML content for inlining into the main report"""
    chart_htmls = {}

    # Prepare chart data for JavaScript
    chart_htmls['chart_data'] = prepare_chart_data(ctx)

    # Load release data for filtering
    chart_htmls['release_data'] = load_release_data(ctx)

    return chart_htmls

def prepare_chart_data(ctx):
    """Prepare data for all charts in a format suitable for JavaScript"""

    # First, calculate baseline (best) times for each query across all revisions
    baseline_query = """
    WITH query_baselines AS (
        SELECT 
            query_name,
            MIN(execution_time) as baseline_time
        FROM benchmark_results 
        WHERE query_type = 'query'
        GROUP BY query_name
    )
    SELECT 
        br.git_revision,
        br.git_revision_timestamp,
        br.query_name,
        br.execution_time,
        qb.baseline_time,
        -- ClickBench normalization: (10ms + query_time) / (10ms + baseline_time)
        (0.01 + br.execution_time) / (0.01 + qb.baseline_time) as normalized_time
    FROM benchmark_results br
    JOIN query_baselines qb ON br.query_name = qb.query_name
    WHERE br.query_type = 'query'
    """

    result = ctx.sql(baseline_query)
    normalized_df = result.to_pandas()

    if len(normalized_df) == 0:
        return {"performance": {}, "queries": {}}

    # Register the normalized results as a temporary table
    normalized_df = ctx.from_pandas(normalized_df)
    ctx.register_view("normalized_results_js", normalized_df)

    # Get performance over time data with normalized times
    performance_query = """
    SELECT 
        git_revision,
        git_revision_timestamp,
        AVG(normalized_time) as avg_time,
        MEDIAN(normalized_time) as median_time
    FROM normalized_results_js
    GROUP BY git_revision_timestamp, git_revision
    ORDER BY git_revision_timestamp, git_revision
    """

    result = ctx.sql(performance_query)
    performance_df = result.to_pandas()

    if len(performance_df) == 0:
        return {"performance": {}, "queries": {}}

    # Convert timestamp to datetime
    performance_df['git_revision_timestamp'] = pd.to_datetime(performance_df['git_revision_timestamp'], utc=True)

    # Sort by timestamp to ensure chronological order for plotting
    performance_df = performance_df.sort_values('git_revision_timestamp')

    # Get per-query performance data with both raw and normalized times
    queries_query = """
    SELECT 
        git_revision,
        git_revision_timestamp,
        query_name,
        MEDIAN(normalized_time) as median_time
    FROM normalized_results_js 
    GROUP BY git_revision, git_revision_timestamp, query_name
    ORDER BY git_revision_timestamp, query_name
    """

    result = ctx.sql(queries_query)
    queries_df = result.to_pandas()

    if len(queries_df) == 0:
        queries_df = pd.DataFrame()
    else:
        queries_df['git_revision_timestamp'] = pd.to_datetime(queries_df['git_revision_timestamp'], utc=True)
        # Sort by timestamp to ensure chronological order for plotting
        queries_df = queries_df.sort_values('git_revision_timestamp')

    # Create Plotly figures for JavaScript consumption
    performance_fig = create_performance_plotly_data(performance_df)
    queries_fig = create_queries_plotly_data(queries_df)

    return {
        "performance": performance_fig,
        "queries": queries_fig
    }

def create_performance_plotly_data(df, normalized=False):
    """Create Plotly data structure for performance chart"""
    if len(df) == 0:
        return {"data": [], "layout": {}, "config": {}}

    # Load revision labels for vertical lines
    labels_path = os.path.join(os.path.dirname(__file__), 'releases.json')
    if os.path.exists(labels_path):
        with open(labels_path, 'r') as f:
            revision_labels = {item['revision']: item['label'] for item in json.load(f)}
    else:
        revision_labels = {}

    # Load event labels for vertical lines
    events_path = os.path.join(os.path.dirname(__file__), 'events.json')
    if os.path.exists(events_path):
        with open(events_path, 'r') as f:
            event_labels = {item['revision']: item['label'] for item in json.load(f)}
    else:
        event_labels = {}

    # Map revision to timestamp for annotation
    rev_to_timestamp = df.groupby('git_revision')['git_revision_timestamp'].min().to_dict()

    # Update names and hover templates for normalized data
    name_prefix = "Normalized "
    hover_metric = "Normalized Ratio: %{y:.3f}"

    data = [
        {
            "x": df['git_revision_timestamp'].dt.strftime('%Y-%m-%dT%H:%M:%S.%fZ').tolist(),
            "y": df['avg_time'].tolist(),
            "mode": "lines+markers",
            "name": f"Average {name_prefix}Time",
            "line": {"color": "green", "width": 2},
            "marker": {"size": 4, "symbol": "triangle-up"},
            "customdata": df['git_revision'].tolist(),
            "hovertemplate": f"<b>Average {name_prefix}Time</b><br>Date: %{{x}}<br>Git SHA: %{{customdata}}<br>{hover_metric}<br><extra></extra>"
        },
        {
            "x": df['git_revision_timestamp'].dt.strftime('%Y-%m-%dT%H:%M:%S.%fZ').tolist(),
            "y": df['median_time'].tolist(),
            "mode": "lines+markers",
            "name": f"Median {name_prefix}Time",
            "line": {"color": "orange", "width": 2},
            "marker": {"size": 4, "symbol": "diamond"},
            "customdata": df['git_revision'].tolist(),
            "hovertemplate": f"<b>Median {name_prefix}Time</b><br>Date: %{{x}}<br>Git SHA: %{{customdata}}<br>{hover_metric}<br><extra></extra>"
        }
    ]

    # Use appropriate scale and title based on whether data is normalized
    y_scale = 'linear'
    y_title = 'Normalized Query Time Ratio'
    chart_title = "Normalized Performance Over Time (ClickBench Definition)"

    layout = {
        "title": chart_title,
        "xaxis": {"title": "Git Revision Timestamp"},
        "yaxis": {"title": y_title, "type": y_scale},
        "hovermode": "x unified",
        "template": "plotly_white",
        "height": 500,
        "margin": {"l": 50, "r": 50, "t": 80, "b": 50},
        "shapes": [],
        "annotations": []
    }

    # Add vertical lines and annotations for labeled revisions (releases)
    for rev, label in revision_labels.items():
        if rev in rev_to_timestamp:
            ts = rev_to_timestamp[rev]
            if hasattr(ts, 'strftime'):
                ts_str = ts.strftime('%Y-%m-%dT%H:%M:%S.%fZ')
            else:
                ts_str = pd.to_datetime(ts).strftime('%Y-%m-%dT%H:%M:%S.%fZ')

            layout["shapes"].append({
                "type": "line",
                "x0": ts_str,
                "x1": ts_str,
                "y0": 0,
                "y1": 1,
                "yref": "paper",
                "line": {
                    "color": "red",
                    "width": 2,
                    "dash": "dash"
                },
                "CUSTOM_ANNOTATION": "release"
            })

            layout["annotations"].append({
                "x": ts_str,
                "y": 1.01,
                "yref": "paper",
                "text": label,
                "showarrow": False,
                "xanchor": "left",
                "yanchor": "bottom",
                "font": {"color": "red", "size": 12},
                "bgcolor": "rgba(255,255,255,0.7)",
                "bordercolor": "red",
                "CUSTOM_ANNOTATION": "release"
            })

    # Add vertical lines and annotations for events (blue lines)
    for rev, label in event_labels.items():
        if rev in rev_to_timestamp:
            ts = rev_to_timestamp[rev];
            if hasattr(ts, 'strftime'):
                ts_str = ts.strftime('%Y-%m-%dT%H:%M:%S.%fZ');
            else:
                ts_str = pd.to_datetime(ts).strftime('%Y-%m-%dT%H:%M:%S.%fZ');

            layout["shapes"].append({
                "type": "line",
                "x0": ts_str,
                "x1": ts_str,
                "y0": 0,
                "y1": 1,
                "yref": "paper",
                "line": {
                    "color": "blue",
                    "width": 2,
                    "dash": "dot"
                },
                "CUSTOM_ANNOTATION": "event"
            })

            layout["annotations"].append({
                "x": ts_str,
                "y": 0.99,
                "yref": "paper",
                "text": f"Event: {label}",
                "showarrow": False,
                "xanchor": "right",
                "yanchor": "top",
                "font": {"color": "blue", "size": 12},
                "bgcolor": "rgba(255,255,255,0.7)",
                "bordercolor": "blue",
                "CUSTOM_ANNOTATION": "event"
            })

    config = {"responsive": True};

    return {"data": data, "layout": layout, "config": config}


def create_queries_plotly_data(df):
    """Create Plotly data structure for individual queries chart"""
    if len(df) == 0:
        return {"data": [], "layout": {}, "config": {}}

    # Load revision labels for vertical lines
    labels_path = os.path.join(os.path.dirname(__file__), 'releases.json')
    if os.path.exists(labels_path):
        with open(labels_path, 'r') as f:
            revision_labels = {item['revision']: item['label'] for item in json.load(f)}
    else:
        revision_labels = {}

    # Load event labels for vertical lines
    events_path = os.path.join(os.path.dirname(__file__), 'events.json')
    if os.path.exists(events_path):
        with open(events_path, 'r') as f:
            event_labels = {item['revision']: item['label'] for item in json.load(f)}
    else:
        event_labels = {}

    # Map revision to timestamp for annotation
    rev_to_timestamp = df.groupby('git_revision')['git_revision_timestamp'].min().to_dict()

    # Get all unique queries and sort them by average execution time for better legend ordering
    # This makes it easier to match legend entries with chart lines
    query_avg_times = df.groupby('query_name')['median_time'].mean().sort_values()
    unique_queries = query_avg_times.index.tolist()
    unique_queries.reverse()

    data = []
    for query_name in unique_queries:
        query_data = df[df['query_name'] == query_name]
        if len(query_data) > 0:
            data.append({
                "x": query_data['git_revision_timestamp'].dt.strftime('%Y-%m-%dT%H:%M:%S.%fZ').tolist(),
                "y": query_data['median_time'].tolist(),
                "mode": "lines+markers",
                "name": query_name,
                "line": {"width": 2},
                "marker": {"size": 4},
                "customdata": query_data['git_revision'].tolist(),
                "hovertemplate": f"<b>{query_name}</b><br>Date: %{{x}}<br>Git SHA: %{{customdata}}<br>Normalized Ratio: %{{y:.4f}}s<br><extra></extra>",
                "visible": True
            });

    y_scale = 'linear'
    y_title = 'Median Normalized Execution Time'

    layout = {
        "title": "Individual Query Performance Over Time",
        "xaxis": {"title": "Git Revision Timestamp"},
        "yaxis": {"title": y_title, "type": y_scale},
        "hovermode": "x unified",
        "template": "plotly_white",
        "height": 600,
        "margin": {"l": 50, "r": 50, "t": 80, "b": 50},
        "shapes": [],
        "annotations": []
    }

    # Add vertical lines and annotations for labeled revisions (releases)
    for rev, label in revision_labels.items():
        if rev in rev_to_timestamp:
            ts = rev_to_timestamp[rev]
            if hasattr(ts, 'strftime'):
                ts_str = ts.strftime('%Y-%m-%dT%H:%M:%S.%fZ')
            else:
                ts_str = pd.to_datetime(ts).strftime('%Y-%m-%dT%H:%M:%S.%fZ')

            layout["shapes"].append({
                "type": "line",
                "x0": ts_str,
                "x1": ts_str,
                "y0": 0,
                "y1": 1,
                "yref": "paper",
                "line": {
                    "color": "red",
                    "width": 2,
                    "dash": "dash"
                },
                "CUSTOM_ANNOTATION": "release"
            })

            layout["annotations"].append({
                "x": ts_str,
                "y": 1.01,
                "yref": "paper",
                "text": label,
                "showarrow": False,
                "xanchor": "left",
                "yanchor": "bottom",
                "font": {"color": "red", "size": 12},
                "bgcolor": "rgba(255,255,255,0.7)",
                "bordercolor": "red",
                "CUSTOM_ANNOTATION": "release"
            });

    # Add vertical lines and annotations for events (blue lines)
    for rev, label in event_labels.items():
        if rev in rev_to_timestamp:
            ts = rev_to_timestamp[rev];
            if hasattr(ts, 'strftime'):
                ts_str = ts.strftime('%Y-%m-%dT%H:%M:%S.%fZ');
            else:
                ts_str = pd.to_datetime(ts).strftime('%Y-%m-%dT%H:%M:%S.%fZ');

            layout["shapes"].append({
                "type": "line",
                "x0": ts_str,
                "x1": ts_str,
                "y0": 0,
                "y1": 1,
                "yref": "paper",
                "line": {
                    "color": "blue",
                    "width": 2,
                    "dash": "dot"
                },
                "CUSTOM_ANNOTATION": "event"
            });

            layout["annotations"].append({
                "x": ts_str,
                "y": 0.99,
                "yref": "paper",
                "text": f"Event: {label}",
                "showarrow": False,
                "xanchor": "right",
                "yanchor": "top",
                "font": {"color": "blue", "size": 12},
                "bgcolor": "rgba(255,255,255,0.7)",
                "bordercolor": "blue",
                "CUSTOM_ANNOTATION": "event"

            })

    return {
        "data": data,
        "layout": layout,
        "config": {
            "responsive": True,
            "displayModeBar": True
        }
    }


def load_release_data(ctx):
    """Load release data for filtering charts"""
    # Load revision labels from releases.json
    labels_path = os.path.join(os.path.dirname(__file__), 'releases.json')
    if os.path.exists(labels_path):
        with open(labels_path, 'r') as f:
            releases_info = json.load(f)
            releases = [item['revision'] for item in releases_info]

            # Find the most recent release
            most_recent_release = None
            if releases_info:
                # Assuming releases.json is ordered by date, take the last one
                most_recent_release = releases_info[-1]['revision']
    else:
        releases = []
        most_recent_release = None

    # Get most recent timestamp from data
    most_recent_query = """
    SELECT MAX(git_revision_timestamp) as most_recent
    FROM benchmark_results
    WHERE query_type = 'query'
    """

    try:
        result = ctx.sql(most_recent_query)
        most_recent_df = result.to_pandas()
        most_recent_timestamp = most_recent_df.iloc[0]['most_recent'] if len(most_recent_df) > 0 else None
    except:
        most_recent_timestamp = None

    return {
        "releases": releases,
        "mostRecent": most_recent_timestamp,
        "mostRecentRelease": most_recent_release
    }


if __name__ == "__main__":
    main()
