#!/usr/bin/env bash
# This uses vim with the default server name.
# If you're using a custom server name, youll need to add --servername to the vim invocation below
# https://vimdoc.sourceforge.net/htmldoc/remote.html#--servername

# Your vim will need to be compiled with +clientserver for this to work.
# If you're using MacVim, you'll want to change vim to mvim

# From https://stackoverflow.com/a/45977232
# Expects a file:// Regex and ignores the hostname
FILE_REGEX='^file://[^/]+(/[^#]+)#(.*)$'
if [[ "$1" =~ $FILE_REGEX ]]; then
  FILE=${BASH_REMATCH[1]}
  LINE=${BASH_REMATCH[2]}
  vim --remote +$LINE $FILE
else
  echo "Couldn't parse $1 as a file regexp"
fi
