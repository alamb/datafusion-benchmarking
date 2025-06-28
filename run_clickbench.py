#!/usr/bin/env python3
import argparse
import subprocess
import time
import platform
import os


# This script runs ClickBench queries using DataFusion datafusion-cli
# and writes the results to a CSV file.
#
# Arguments
# --output-dir: Directory to write results
# --git-revision: Git revision of the DataFusion repository (optional)
# --git-revision-date: Date of the git revision (optional)
# --datafusion-version: Version of DataFusion (optional)

def main():
    parser = argparse.ArgumentParser(description="Run ClickBench queries with DataFusion.")
    parser.add_argument('--output-dir', help='Directory to write output files', default="results")
    parser.add_argument('--git-revision', help='Git revision of the DataFusion repository')
    parser.add_argument('--git-revision-timestamp', help='Date of the git revision')
    parser.add_argument('--datafusion-binary', help='Path to datafusion-cli binary', default='datafusion-cli')
    args = parser.parse_args()

    output_dir = args.output_dir
    if not os.path.exists(output_dir):
        os.makedirs(output_dir)
    print(f"Output will be written to: {output_dir}")

    script_start_timestamp = time.strftime("%Y-%m-%d %H:%M:%S", time.localtime())
    results = []
    # note these queries are from the DataFusion ClickBench benchmark
    # `cp -R ~/Software/datafusion/benchmarks/queries/clickbench queries/`

    for query in range(0, 43):
        query = f'q{query}'
        results.extend(run_clickbench_query(query, args, script_start_timestamp))

    # Now write the output to a csv file in the output directory using the csv module
    output_file = os.path.join(output_dir, f'{script_start_timestamp}-results.csv')
    print(f"Writing results to {output_file}")
    with open(output_file, 'w') as f:
        # write a header row using all keys from the first result
        columns = results[0].keys()
        f.write(','.join(columns))
        f.write('\n')
        # write the results in the same order
        for results in results:
            f.write(','.join(str(results[col]) for col in columns))
            f.write('\n')

# runs the specified ClickBench query using DataFusion
# and returns a list of results.
# example query names: q2, q3
# results is a list of dictionaries
def run_clickbench_query(query_name, args, script_start_timestamp):
    print(f"Running Query: {query_name}")
    query_directory = os.path.join(os.path.dirname(__file__), 'queries')
    query_file = os.path.join(query_directory, 'clickbench', 'queries', f'{query_name}.sql')
    num_runs = 5

    # Execute the command, timing how long it takes and then writing the result to the output
    # prepare a temporary script file  in a temporary directory
    try:
        # read query_file into a string
        with open(query_file, 'r') as f:
            query_content = f.read()

        # Create a temporary script file to set the configuration
        # from https://github.com/ClickHouse/ClickBench/blob/main/datafusion/create_partitioned.sql
        temp_dir = os.path.join(os.path.dirname(__file__), 'tmp')
        if not os.path.exists(temp_dir):
            os.makedirs(temp_dir)

        temp_script = os.path.join(temp_dir, 'script.sql')
        with open(temp_script, 'w') as f:
            f.write("""
            CREATE EXTERNAL TABLE hits
            STORED AS PARQUET
            LOCATION 'data/hits_partitioned/'
            OPTIONS ('binary_as_string' 'true');
            """)
            # write the query multiple times to gather multiple results
            for i in range(0, num_runs):
                f.write(f"{query_content}")


        # Now execute the command with the temporary script
        # and time how long it takes to run the whole thing
        command = f"{args.datafusion_binary} -f {temp_script}"
        #print(f"Executing command: {command}")
        start_time = time.time()
        result = subprocess.run(command, shell=True, capture_output=True, text=True, check=True)
        end_time = time.time()

        elapsed_time = end_time - start_time

        # TODO: figure out a way to check for errors
        # if "Error" in result.stdout or "Error" in result.stderr:
        #     print("An error occurred during query execution.")
        #     print(result.stdout)
        #     print(result.stderr)
        #     return []

        print(f"Total execution took {elapsed_time} seconds.")

        # find all lines like this and extract the numeric value:
        # Elapsed 0.023 seconds.
        timings = []
        for line in result.stdout.splitlines():
            if "Elapsed" in line:
                parts = line.split()
                if len(parts) >= 3:
                    try:
                        timing = float(parts[1])
                        timings.append(timing)
                    except ValueError:
                        print(f"Could not convert timing to float: {parts[1]}")


        #print("Timings for each run:")
        for i, timing in enumerate(timings):
            print(f"Run {i + 1}: {timing}")

        results = []
        for i, timing in enumerate(timings):
            results.append({
                "benchmark_name": "clickbench_partitioned",
                "query_name": query_name,
                "query_type": "query" if i != 0 else "table_creation",
                "execution_time": timing,
                "run_timestamp": script_start_timestamp,
                "git_revision": args.git_revision if args.git_revision is not None else "",
                "git_revision_timestamp": args.git_revision_timestamp if args.git_revision_timestamp is not None else "",
                "num_cores": os.cpu_count(),
                #"cpu_model": platform.processor(),
                #"os": platform.system(),
                #"os_version": platform.version(),

            })
        return results

    except subprocess.CalledProcessError as e:
        print(f"Error executing query: {e.stderr}")


if __name__ == "__main__":
    main()
