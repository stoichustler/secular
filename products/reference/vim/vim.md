# Vim development environment

This note covers installing Vim on a Linux host and using it with Secular's
Zircon, C/C++, Rust, assembly, GN, and Markdown files. Vim runs on the host.
It is not packaged into Secular and does not depend on the target QEMU image.

## Version and directory layout

Source lives under `~/opt/vim-src` by default; the install prefix is `~/opt`.
A pinned tag makes editor behavior easier to reproduce across machines:

```sh
mkdir -p ~/opt
git clone https://github.com/vim/vim.git ~/opt/vim-src
cd ~/opt/vim-src

# Optional: replace <tag> with the team's agreed Vim tag
git fetch --tags --depth=1
git checkout <tag>
```

Check the host toolchain before compiling:

```sh
git --version
cc --version
make --version
python3 --version
```

## Install dependencies

The packages below match the current build: terminal Vim with Python 3, Perl,
Lua, X11, and cscope. If you only need a terminal editor, drop the X11
packages and the `--with-x` configure flag.

```sh
sudo apt update
sudo apt install -y \
  build-essential \
  libncurses-dev \
  libx11-dev \
  libxt-dev \
  libsm-dev \
  libice-dev \
  libxpm-dev \
  python3-dev \
  libperl-dev \
  liblua5.4-dev \
  cscope \
  universal-ctags
```

`python3-config` usually comes with `python3-dev`. Confirm that Vim's
configure script can find the Python config directory:

```sh
python3-config --configdir
```

## Build and install

Run these from the source directory. `--prefix=$HOME/opt` does not write into
system paths, so `sudo make install` is not required:

```sh
cd ~/opt/vim-src

./configure \
  --with-features=huge \
  --enable-multibyte \
  --enable-python3interp=yes \
  --with-python3-config-dir="$(python3-config --configdir)" \
  --enable-perlinterp=yes \
  --enable-luainterp=yes \
  --enable-cscope \
  --enable-gui=no \
  --with-x \
  --prefix="$HOME/opt"

make -j"$(nproc)"
make install
```

Put the install directory on the current shell's `PATH`:

```sh
export PATH="$HOME/opt/bin:$PATH"
```

To keep that change, add the same line to `~/.profile` or your shell startup
file, then open a new terminal. Confirm that the new Vim is the one that
runs:

```sh
command -v vim
vim --version | sed -n '1,8p'
vim --clean --not-a-term -Nu NONE -n +'quit'
```

The last command checks that Vim starts with no config and no interactive
terminal. Version output and exit code 0 mean the base install is working.

## Install the Secular config

Run these from the Secular repository root. Back up any existing config
first. `.vimrc` creates `~/.vim/swap` and `~/.vim/undo`; it does not write
recovery files into the source tree:

```sh
[ -f "$HOME/.vimrc" ] && cp "$HOME/.vimrc" "$HOME/.vimrc.bak.$(date +%Y%m%d%H%M%S)"
install -Dm644 products/reference/vim/.vimrc "$HOME/.vimrc"
```

Useful commands from that config:

| Command | Purpose |
| --- | --- |
| `:BEAUStyle` | Secular/Hypervisor style: 8-column tabs |
| `:GOOGLEStyle` | Google C++ style: 2 spaces |
| `:LINUXStyle` | Linux kernel style: 8-column tabs |
| `:NERDTreeToggle` | Open or close the file tree |
| `:Files` | Find files with fzf |
| `:Rg <pattern>` | Search source with fzf/rg |
| `:Tags <symbol>` | Look up a ctags symbol |

The C/C++ style commands affect only the current buffer. After opening a
file, `gg=G` reindents existing content to the current style.

## Install plugins

Vim's native package path autoloads plugins under `~/.vim/pack/*/start/*` at
startup. Create the directories, then install the plugins that `.vimrc`
expects:

```sh
mkdir -p ~/.vim/pack/plugins/start ~/.vim/pack/themes/start

git clone https://github.com/preservim/nerdtree.git \
  ~/.vim/pack/plugins/start/nerdtree
git clone https://github.com/jiangmiao/auto-pairs.git \
  ~/.vim/pack/plugins/start/auto-pairs
git clone https://github.com/preservim/nerdcommenter.git \
  ~/.vim/pack/plugins/start/nerdcommenter
git clone https://github.com/tpope/vim-surround.git \
  ~/.vim/pack/plugins/start/vim-surround
git clone https://github.com/vim-airline/vim-airline.git \
  ~/.vim/pack/plugins/start/vim-airline
git clone https://github.com/folke/tokyonight.vim.git \
  ~/.vim/pack/themes/start/tokyonight
git clone --depth 1 https://github.com/junegunn/fzf.git ~/.fzf
~/.fzf/install --all
git clone https://github.com/junegunn/fzf.vim.git \
  ~/.vim/pack/plugins/start/fzf
```

A second `git clone` fails if the directory already exists. Upgrade an
installed plugin with `git -C <plugin-dir> pull --ff-only`. Offline hosts can
copy the same directories onto the same package paths. After starting Vim,
check that plugins and the colorscheme loaded:

```vim
:scriptnames
:echo has('python3')
:echo exists('*fzf#vim#files')
:colorscheme tokyonight
```

If `colorscheme tokyonight` errors, confirm that
`~/.vim/pack/themes/start/tokyonight/colors/tokyonight.vim` exists, or
temporarily comment out `colorscheme tokyonight` in `.vimrc` while diagnosing
the rest of the config.

After Vim config or plugins change, run at least one no-config startup check,
one target-architecture build, and the matching verification. This note only
describes the host editor. It does not claim that plugins work offline or
across every Vim version.

---

Everyday's a Hustle.
