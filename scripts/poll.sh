#!/bin/bash
#
# Script looks for scripts in the jobs directory and runs then
# on completion renames to `job.done` to avoid rerunning
#
# AKA ghetto job scheduler

set -x -e

mkdir -p jobs
while true ; do
    echo "Checking for jobs"
    for job in $(ls -tr jobs/*.sh 2>/dev/null) ; do
        echo "Running job $job"
        setsid bash "$job" &
        JOB_PID=$!
        echo "$(date -u +"%Y-%m-%dT%H:%M:%SZ") $JOB_PID" > "${job}.started"
        wait $JOB_PID 2>/dev/null || true
        # Job file may have been removed by cancel
        if [ -f "$job" ]; then
            echo "Renaming $job to $job.done"
            mv -f "$job" "$job.done"
        fi
        rm -f "${job}.started"
    done
    sleep 1
done
