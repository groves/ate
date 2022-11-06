`ate` is a terminal pager that parses [terminal hyperlinks] and lets you search, move between, and open them.
It navigates in addition to paginating.
While it pages through text streams like existing terminal pagers, [less] has far more features for that use case.

[terminal hyperlinks]: https://gist.github.com/egmontkob/eb114294efbcd5adb1944c9f3cb5feda
[less]: https://github.com/gwsw/less

Watch the video below for high-level usage and check out [why ate] for more on the motivation for the tool and how to use it.

[why ate]: https://sevorg.org/posts/why_ate/


https://user-images.githubusercontent.com/12270/194925821-a69e749d-798f-4702-b064-1a2978aeb8f3.mp4

Unmute the video to hear narration or read it below:

* Run [ripgrep] on the term `render` in this repo. We wrap ripgrep with [hyperer] to insert links to the matched files
* Mouse over the links to show what was inserted
* Rerun ripgrep feeding the output into `ate`
* Step through the links with `n`
* Search with `/` and whittle down to a single result
* Hit `Enter` to open that match in our editor

Installation
============
[Install Rust], clone this repo, and run `cargo install` in your clone.
Alternatively, if you're using the [Nix package manager], depend on the flake.nix in this repo.

[Install Rust]: https://www.rust-lang.org/tools/install
[Nix package manager]: https://nixos.org/manual/nix/stable/introduction.html

You may also want to install something to [get links] if you don't have a program you want to use with `ate`.

[get links]: #getting-links

Usage
=====
Send text to `ate`'s standard input, like via this pipe:
`hyperer-rg --pretty <my_search> | ate`

Or like this input redirection:

`ate < my_linkful_output`
 
In either case, `ate` will show the first screenful of text and parse any links in it.

Key Bindings
------------
* `n` goes to the next link.
* `N` goes to the previous one.
* `Enter` opens the currently selected link by starting the command in the `ATE_OPENER` environment variable with the link address as the first argment.
* `/` opens a link searcher and typing text there reduces the links to ones that contain the typed text.
* ⬆️ and ⬇️ move forward and backwards in matches in the link searcher.
* `Enter` in the link searcher selects the current link there and returns to the text view.
* `Esc` in the link searcher exits searching and returns to the position before searching.
* `q` exits in normal mode and `Ctrl-C` exits in any mode.

Environment Variables
---------------------
All of `ate`'s configuration is done through environment variables:

### `ATE_OPENER`
Program to invoke to open a link e.g. when `Enter` is pressed. 
The selected link is passed to it as the first argument.
The link should be of the form `file://hostname/path#line number` according to [the terminal hyperlinks doc][terminal hyperlinks].
There's no guarantee that a program isn't emitting malformed links, but `ate` openers assume that form for now.

`ate` expects to invoke this process and for it to open the file to edit in another window.
For example, you can use [Vim's remote command][Vim remote] or [emacsclient] to do that.

[Vim remote]: https://vimdoc.sourceforge.net/htmldoc/remote.html#--remote
[emacsclient]: https://www.gnu.org/software/emacs/manual/html_node/emacs/Invoking-emacsclient.html

[opener_examples] has scripts that can be used as openers.
To use one, download it, modify it if your system differs, and export `ATE_OPENER` as the full path to the script.

[opener_examples]: https://github.com/groves/ate/tree/main/opener_examples

For example, if you have this repo cloned at `~/dev/ate` and use NeoVim, you run a different command depending on your shell:
* For `zsh` or `bash`: `export ATE_OPENER=~/dev/ate/opener_examples/nvim_opener.bash`
* For `fish`: `set -xU ATE_OPENER ~/dev/ate/opener_examples/nvim_opener.bash`

For `zsh` or `bash`, you'll also want to add it to your shell startup files to get it to show up in new shells.

### `ATE_OPEN_FIRST`
If defined, `ate` will open the first link it finds on starting.
I use this Bash script to run `cargo` and compile Rust:

```bash
# Use hyperer-cargo to link to Rust files in compilation failures, test failures, and backtraces
hyperer-cargo --color=always $* |\
# Print the cargo output to the terminal and to a temp file
  tee /tmp/hyperlinked_cargo.out

# If the cargo command failed, open the output in ate.
# If it didn't fail, we won't have anything interesting to navigate
if [[ ${PIPESTATUS[0]} -ne 0 ]] ; then
  # If it's been under 5 seconds since the script started, set ATE_OPEN_FIRST
  # This means if a compile or test was quick, we open the failure using ATE_OPENER ASAP
  # If cargo took longer, we don't immediately open a link in case we've started doing something in our editor.
  if [[ $SECONDS -lt 5  ]] ; then
    export ATE_OPEN_FIRST=
  fi
  ate < /tmp/hyperlinked_cargo.out
fi
```

Getting Links
=============
`ate`'s most useful on text containing hyperlinks.
Terminal hyperlinks are a relatively new feature, so few programs support them out of the box.
`ls`, `gcc`, `systemd`, and [delta] are some that do.

[delta]: https://github.com/dandavison/delta

Until hyperlink support shows up in more programs, 
we can wrap existing programs, detect things that could be linked in their output, and emit terminal links around that text.

[hyperer] does that for [ripgrep] and [cargo].
I highly recommend installing it and using ripgrep with it to get a sense of what `ate` does.
It can also serve as a base for adding links to other commands.

[hyperer]: https://github.com/groves/hyperer
[ripgrep]: https://github.com/BurntSushi/ripgrep
[cargo]: https://doc.rust-lang.org/cargo/

What's Missing
==============
`ate` is very young and is missing obvious features. I plan to add at least these:
* Searching for text. It only searches links currently.
* Streaming input. It currently reads all of standard input on startup.

It might also make sense to add these features:
* Taking file arguments instead of only standard input.
* Tailing files.

It's possible that it'll be possible to handle these cases with other utilities.
If it isn't, I'll add them to ate, too.
