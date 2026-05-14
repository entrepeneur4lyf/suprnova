#!/bin/bash

# Kill background processes on exit
trap 'kill $(jobs -p) 2>/dev/null' EXIT

# Kill any existing processes on our ports
lsof -ti:5173 | xargs kill -9 2>/dev/null
lsof -ti:8080 | xargs kill -9 2>/dev/null

# Check if node_modules exists, if not install
if [ ! -d "app/frontend/node_modules" ]; then
    echo "Installing frontend dependencies..."
    (cd app/frontend && npm install)
fi

# Generate TypeScript types from InertiaProps
echo "Generating TypeScript types..."
(cd app && ../target/debug/suprnova generate-types 2>/dev/null || true)

# Start Vite dev server in background
echo "Starting Vite dev server on http://localhost:5173..."
(cd app/frontend && npm run dev) &

# Give Vite a moment to start
sleep 2

# Start Suprnova server
echo "Starting Suprnova server on http://localhost:8080..."
cargo run -p app
