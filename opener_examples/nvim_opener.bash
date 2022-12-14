#!/usr/bin/env bash
# Uses https://neovim.io/doc/user/remote.html
# If you're using a different address, modify this variable
NVIM_LISTEN_ADDRESS=~/.cache/nvim/server.pipe 

if [[ ! -e $NVIM_LISTEN_ADDRESS ]] ; then
  echo "Start nvim as 'nvim --listen $NVIM_LISTEN_ADDRESS' separately to let ate open files remotely"
  exit 1
fi

# From https://stackoverflow.com/a/45977232
# Expects a file:// Regex and ignores the hostname
FILE_REGEX='^file://[^/]+(/[^#]+)#([^:]+)(:(.*))?$'
if [[ "$1" =~ $FILE_REGEX ]]; then
  FILE=${BASH_REMATCH[1]}
  LINE=${BASH_REMATCH[2]}
  COL=1
  if [[ ${BASH_REMATCH[4]} ]]; then
    COL=${BASH_REMATCH[4]}
  fi
  nvim --server $NVIM_LISTEN_ADDRESS --remote-send ":edit $FILE<CR>:call cursor ($LINE, $COL)<CR>"
else 
  echo "Couldn't parse $1 as a file regexp"
fi
