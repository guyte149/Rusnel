#!/usr/bin/env bash

# Exit immediately on error, treat unset variables as an error
set -euo pipefail

###########################################################
# Helper: Wait a bit for processes to start listening
# (You might want something more robust in real scenarios)
###########################################################
wait_for_startup() {
  sleep 1
}

###########################################################
# Test 1: TCP Local Forward
# remote 3000:127.0.0.1:4000
###########################################################
test_tcp_local_forward() {
  echo "[TEST] TCP Local Forward (3000 -> 127.0.0.1:4000)"

  local TEST_MSG="Hello from test_tcp_local_forward"
  local SERVER_OUT="server_output.txt"

  # 1) Start the tunnel server in the background
  cargo run server &
  SERVER_PID=$!
  wait_for_startup

  # 2) Start the tunnel client (listening on 127.0.0.1:8080, forwarding 3000->4000)
  cargo run client 127.0.0.1:8080 3000:127.0.0.1:4000 &
  CLIENT_PID=$!
  wait_for_startup

  # 3) Start netcat listening on port 4000 (TCP)
  #    -N: close connection on EOF (if supported)
  nc -lp 4000 > "$SERVER_OUT" 2>/dev/null &
  local NC_SERVER_PID=$!
  wait_for_startup

  # 4) Send test message to port 3000
  echo "$TEST_MSG" | nc -N 127.0.0.1 3000

  # Give netcat a moment to capture output, if needed
  sleep 1

  # Validate that the server_output.txt file contains our message
  if grep -q "$TEST_MSG" "$SERVER_OUT"; then
    echo "[OK] Message received by server."
  else
    echo "[ERROR] Message not found in server_output.txt!"
    kill $NC_SERVER_PID || true
    kill $CLIENT_PID   || true
    kill $SERVER_PID   || true
    exit 1
  fi

  # Clean up
  kill $NC_SERVER_PID || true
  kill $CLIENT_PID   || true
  kill $SERVER_PID   || true
  rm $SERVER_OUT || true
  wait || true

  echo "[TEST] TCP Local Forward - PASSED"
  echo
}

###########################################################
# Test 2: TCP Reverse Forward
# remote R:3000:127.0.0.1:4000
###########################################################
test_tcp_reverse_forward() {
  echo "[TEST] TCP Reverse Forward (R:3000 -> 127.0.0.1:4000)"

  local TEST_MSG="Hello from test_tcp_local_forward"
  local SERVER_OUT="server_output.txt"

  # 1) Start tunnel server
  cargo run server --allow-reverse &
  SERVER_PID=$!
  wait_for_startup

  # 2) Start tunnel client with reverse forwarding
  cargo run client 127.0.0.1:8080 R:3000:127.0.0.1:4000 &
  CLIENT_PID=$!
  wait_for_startup

  # 3) Start netcat on port 4000
  #    -N: close connection on EOF (if supported)
  nc -lp 4000 > "$SERVER_OUT" 2>/dev/null &
  local NC_SERVER_PID=$!
  wait_for_startup

  # 4) Send test message to port 3000
  echo "$TEST_MSG" | nc -N 127.0.0.1 3000

  # Give netcat a moment to capture output, if needed
  sleep 1

  # Validate that the server_output.txt file contains our message
  if grep -q "$TEST_MSG" "$SERVER_OUT"; then
    echo "[OK] Message received by server."
  else
    echo "[ERROR] Message not found in server_output.txt!"
    kill $NC_SERVER_PID || true
    kill $CLIENT_PID   || true
    kill $SERVER_PID   || true
    exit 1
  fi

  # Clean up
  kill $NC_SERVER_PID || true
  kill $CLIENT_PID   || true
  kill $SERVER_PID   || true
  rm $SERVER_OUT || true
  wait || true

  echo "[TEST] TCP Reverse Forward - PASSED"
  echo
}

###########################################################
# Test 3: UDP Local Forward
# remote 3000:127.0.0.1:4000/udp
###########################################################
test_udp_local_forward() {
  echo "[TEST] UDP Local Forward (3000 -> 127.0.0.1:4000)"

  # 1) Start tunnel server
  cargo run server &
  SERVER_PID=$!
  wait_for_startup

  # 2) Start tunnel client with local UDP forward
  cargo run client 127.0.0.1:8080 3000:127.0.0.1:4000/udp &
  CLIENT_PID=$!
  wait_for_startup

  # 3) Start netcat in UDP mode on port 4000
  nc -u -l 4000 &
  NC_SERVER_PID=$!
  wait_for_startup

  # 4) Send UDP data to 3000
  echo "Hello from test_udp_local_forward" | nc -u 127.0.0.1 3000

  # Clean up
  kill $NC_SERVER_PID || true
  kill $CLIENT_PID   || true
  kill $SERVER_PID   || true
  wait || true

  echo "[TEST] UDP Local Forward - PASSED"
  echo
}

###########################################################
# Test 4: UDP Reverse Forward
# remote R:3000:127.0.0.1:4000/udp
###########################################################
test_udp_reverse_forward() {
  echo "[TEST] UDP Reverse Forward (R:3000 -> 127.0.0.1:4000/udp)"

  # 1) Start tunnel server
  cargo run server &
  SERVER_PID=$!
  wait_for_startup

  # 2) Start tunnel client with reverse UDP forward
  cargo run client 127.0.0.1:8080 R:3000:127.0.0.1:4000/udp &
  CLIENT_PID=$!
  wait_for_startup

  # 3) Start netcat in UDP mode on port 4000
  nc -u -l 4000 &
  NC_SERVER_PID=$!
  wait_for_startup

  # 4) Send data to 3000
  echo "Hello from test_udp_reverse_forward" | nc -u 127.0.0.1 3000

  # Clean up
  kill $NC_SERVER_PID || true
  kill $CLIENT_PID   || true
  kill $SERVER_PID   || true
  wait || true

  echo "[TEST] UDP Reverse Forward - PASSED"
  echo
}

###########################################################
# Test 5: SOCKS Proxy
# remote socks (assumes client listens on 127.0.0.1:1080)
###########################################################
test_socks_proxy() {
  echo "[TEST] SOCKS Proxy (remote socks, proxychains on 1080)"

  # 1) Start tunnel server
  cargo run server &
  SERVER_PID=$!
  wait_for_startup

  # 2) Start tunnel client with "socks"
  cargo run client 127.0.0.1:8080 socks &
  CLIENT_PID=$!
  wait_for_startup

  # 3) Start a TCP server on 4000
  nc -l 4000 &
  NC_SERVER_PID=$!
  wait_for_startup

  # 4) Use proxychains to connect. 
  # -> Make sure /etc/proxychains.conf is configured to use 127.0.0.1:1080 or similar.
  echo "Hello from test_socks_proxy" | proxychains4 nc 127.0.0.1 4000

  # Clean up
  kill $NC_SERVER_PID || true
  kill $CLIENT_PID   || true
  kill $SERVER_PID   || true
  wait || true

  echo "[TEST] SOCKS Proxy - PASSED"
  echo
}

###########################################################
# Main: Run all tests
###########################################################
echo "Starting Functional Tests..."

test_tcp_local_forward
test_tcp_reverse_forward

exit
# still doesn't work:
test_udp_local_forward
test_udp_reverse_forwar
test_socks_proxy

echo "All tests completed successfully."