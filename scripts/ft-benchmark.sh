#!/bin/bash
set -xeo pipefail

# For debugging purposes
whoami
date

# Otherwise nearup and cargo don't work even if installed properly
PATH=/home/ubuntu/.local/bin/:$PATH
export PATH=$PATH:$HOME/.cargo/bin

# Fetch the latest changes from the remote
git fetch

# Check if the local master branch is up to date with the remote master branch
LOCAL=$(git rev-parse master)
REMOTE=$(git rev-parse origin/master)

if [ $LOCAL = $REMOTE ]; then
    echo "The repository is up to date with the remote. No rebuilds or restarts needed."
    exit 0
else
    echo "The local repository is behind the remote. Pulling changes..."
    git pull
fi

# Find all data senders running the specified script
python_processes=$(pgrep -f "scripts/ft-benchmark-data-sender.py")

# Send SIGTERM to data senders
for pid in $python_processes; do
    echo "Sending SIGTERM to Python process with PID $pid"
    kill -SIGTERM $pid
done

# Give data senders time to send data and finish execution
sleep 10

# Stop previous experiment
pkill -9 locust || true
nearup stop 

make neard

# Start neard
nearup run localnet --binary-path target/release/ --num-nodes 1 --num-shards 1 --override
python3 scripts/ft-bechmark-data-sender.py
# Prepare python environment
python3 -m venv .venv
source .venv/bin/activate
python -m pip install -r pytest/requirements.txt
python -m pip install locust
export KEY=~/.near/localnet/node0/validator_key.json

# Run benchmark
cd pytest/tests/loadtest/locust/
locust -H 127.0.0.1:3030  -f locustfiles/ft.py --funding-key=$KEY -u 1000 -r 10 --processes 8 --headless
