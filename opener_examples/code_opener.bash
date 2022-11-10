#!/usr/bin/env bash
# This expects Visual Studio's 'code' tool to be on the path
# See https://code.visualstudio.com/docs/editor/command-line#_launching-from-command-line for installation instructions

# From https://stackoverflow.com/a/45977232
# Expects a file:// Regex and ignores the hostname
FILE_REGEX='^file://[^/]+(/[^#]+)#(.*)$'
if [[ "$1" =~ $FILE_REGEX ]]; then
  FILE=${BASH_REMATCH[1]}
  LINE_AND_MAYBE_COL=${BASH_REMATCH[2]}
  # code will take a colon delimited column, so we can let it sort out if there's a column or not
  code -g $FILE:$LINE_AND_MAYBE_COL
else
  echo "Couldn't parse $1 as a file regexp"
fi
