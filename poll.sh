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
    for job in `ls jobs/*.sh` ; do
        echo "Running job $job"
        bash "$job"
        echo "Renaming $job to $job.done"
        mv -f "$job" "$job.done"
    done
    sleep 1
done;
