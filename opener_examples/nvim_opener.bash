#!/usr/bin/env bash
# Start nvim as `nvim --listen ~/.cache/nvim/server.pipe` or modify NVIM_LISTEN_ADDRESS to where you're listening
NVIM_LISTEN_ADDRESS=~/.cache/nvim/server.pipe 

# From https://stackoverflow.com/a/45977232
# Expects a file:// Regex and ignores the hostname
FILE_REGEX='^file://[^/]+(/[^#]+)#(.*)$'
if [[ "$1" =~ $FILE_REGEX ]]; then
  FILE=${BASH_REMATCH[1]}
  LINE=${BASH_REMATCH[2]}
  nvim --server $NVIM_LISTEN_ADDRESS  --remote-send ":edit +$LINE $FILE<CR>"
else 
  echo "Couldn't parse $1 as a file regexp"
fi
