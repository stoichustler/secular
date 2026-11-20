" MICRO / SMOS host Vimrc (not shipped in the target image).
"
" Source of truth: vision/reference/vim/.vimrc
" Install:         install -Dm644 vision/reference/vim/.vimrc ~/.vimrc
" Reload:          :source $MYVIMRC
" Related notes:   vision/reference/vim/vim.md
"
" This file is reload-safe: jobs, timers, and popups from a previous
" :source are stopped before their script-local state is recreated.
" Public identifiers use the micro_ / Micro prefix.
"
" Contents
"   1. Startup, leader, runtime packs
"   2. Core buffer / window options
"   3. Colorscheme
"   4. Indentation and C/C++ style (path auto + :MICROStyle)
"   5. File defaults (formatprg, gn/fidl, last-position, autoread)
"   5a. Large / AEE *_RAW dumps (no wrap/syntax/list/search-scan)
"   6. Clipboard (X11/Wayland, OSC 52, W23)
"   7. Display, search, undo, completion
"   8. Statusline and async git branch
"   9. Command-line UI
"  10. Floating terminal
"  11. fzf / live ripgrep
"  12. Git signs
"  13. Plugin knobs (NERDTree, netrw, Tagbar, NERDCommenter)
"  14. Commands and Space-leader maps
"
" Compatibility and encoding must come first; nocompatible can reset options.
set nocompatible
set encoding=utf-8
scriptencoding utf-8

" Drop leftover state from a previous :source of the older beau-prefix
" vimrc. New augroups would otherwise stack with the old ones, and already
" open dump buffers would keep b:beau_* flags that the new code ignores.
function! s:RetireLegacyPrefix() abort
    for l:group in [
        \ 'beau_file_defaults', 'beau_large_files',
        \ 'beau_statusline_colors', 'beau_git_branch_status',
        \ 'beau_cmdline_ui', 'beau_floating_terminal',
        \ 'beau_fzf_terminal_keys', 'beau_git_signs',
        \ 'beau_nerdtree_paths']
        if exists('#' . l:group)
            execute 'autocmd!' l:group
            execute 'augroup!' l:group
        endif
    endfor

    if exists('g:beau_workspace_root')
        if !exists('g:micro_workspace_root')
            let g:micro_workspace_root = g:beau_workspace_root
        endif
        unlet g:beau_workspace_root
    endif

    for l:info in getbufinfo()
        let l:buf = l:info.bufnr
        for [l:old, l:new] in [
            \ ['beau_c_style', 'micro_c_style'],
            \ ['beau_large_file', 'micro_large_file'],
            \ ['beau_long_text', 'micro_long_text']]
            let l:val = getbufvar(l:buf, l:old)
            if type(l:val) != v:t_string || l:val !=# ''
                if empty(getbufvar(l:buf, l:new))
                    call setbufvar(l:buf, l:new, l:val)
                endif
            endif
        endfor
    endfor

    if has('signs')
        silent! call sign_unplace('beau_git')
        for l:sign in ['BeauGitAdd', 'BeauGitChange', 'BeauGitDelete']
            if !empty(sign_getdefined(l:sign))
                call sign_undefine(l:sign)
            endif
        endfor
    endif
endfunction
call s:RetireLegacyPrefix()

" Space is the leader for project-style commands.
" Map Space to <Nop> so a lone Space does not move the cursor while waiting
" for timeoutlen to decide whether a leader sequence follows. Prefix chords:
" Space-f* (find), Space-yr/ya, Space-ip, Space-re.
let mapleader = ' '
let maplocalleader = '\'
nnoremap <Space> <Nop>
xnoremap <Space> <Nop>

" Bundled optional packs from the Vim runtime, not third-party plugins.
packadd! matchit        " % matches if/endif, HTML tags, etc.
packadd! editorconfig   " per-project indent from .editorconfig
packadd! hlyank         " brief highlight after yank
packadd! cfilter        " :Cfilter / :Lfilter on quickfix/location lists

" Core buffer and navigation behavior
set autoread
set hidden
set magic
set number
set relativenumber
if exists('+signcolumn')
    set signcolumn=yes
endif
" Mouse support in normal, visual, insert, and terminal modes.
" SGR is needed for columns beyond 223 in modern terminals.
set mouse=a
if has('mouse_sgr')
    set ttymouse=sgr
endif
set scrolloff=10
set sidescrolloff=5
set splitbelow
set splitright
if exists('+smoothscroll')
    set smoothscroll
endif
if exists('+splitkeep')
    set splitkeep=screen
endif
set nostartofline
set virtualedit=block
set confirm
set belloff=all
set history=1000
set undolevels=5000
set nrformats-=octal
set display+=lastline
set switchbuf=useopen,usetab
if exists('+jumpoptions')
    set jumpoptions=stack
endif
set updatetime=400
" timeoutlen: mapped chords (Space-f*, Space-yr, Space-re). 400ms is short
" enough that a completed map (Space-e, Space-f hint) does not feel lagged,
" long enough to type fe/fg/fr. ttimeoutlen: Esc / keycodes only.
set timeout
set ttimeout
set timeoutlen=400
set ttimeoutlen=20

" --- Colorscheme -------------------------------------------------------------
" TokyoNight theme; transparent areas inherit terminal opacity.
" Apply the colorscheme after syntax so highlight groups are not reset.
let g:tokyonight_style = 'storm' " available: night, storm
let g:tokyonight_enable_italic = 0
let g:tokyonight_transparent_background = 1
filetype plugin indent on
if !exists('g:syntax_on')
    syntax enable
endif
if exists('+termguicolors')
    set termguicolors
endif
set background=dark
" Re-applying the colorscheme on :source flashes and resets overlay groups.
if get(g:, 'colors_name', '') !=# 'tokyonight'
    try
        colorscheme tokyonight
    catch
        silent! colorscheme default
    endtry
endif

" --- Indentation -------------------------------------------------------------
" Filetype-aware indentation with a four-space global fallback.
set autoindent
set tabstop=4
set shiftwidth=4
set softtabstop=4
set expandtab
set shiftround

" --- C/C++ indent profiles ---------------------------------------------------
" Buffer-local only. :MICROStyle / :GOOGLEStyle / :LINUXStyle, or automatic
" from the file path (zircon/ → tabs 8, userspace/ → spaces 2). A manual
" command wins for that buffer. Run gg=G after changing a profile.
" a:000[0] is truthy for a quiet autocmd apply (no echo).
function! s:ApplyCStyle(style, ...) abort
    setlocal autoindent cindent nosmartindent
    setlocal nocopyindent nopreserveindent

    if a:style ==# 'hypervisor'
        setlocal tabstop=8 shiftwidth=8 softtabstop=8 noexpandtab
        setlocal cinoptions=:0,l1,g4,h4,N0,t0,+8,(8,W8
        let l:name = 'Hypervisor/MICRO (tabs 8)'
    elseif a:style ==# 'google'
        setlocal tabstop=2 shiftwidth=2 softtabstop=2 expandtab
        setlocal cinoptions=:s,l1,g1,h1,N-s,t0,+4,(4,W4
        let l:name = 'Google C++ (spaces 2)'
    elseif a:style ==# 'linux'
        setlocal tabstop=8 shiftwidth=8 softtabstop=8 noexpandtab
        setlocal cinoptions=:0,l1,g0,h8,N0,t0,+8,(0,w1,W8
        let l:name = 'Linux kernel (tabs 8)'
    else
        echoerr 'Unknown C/C++ style: ' . a:style
        return
    endif

    let b:micro_c_style = a:style
    if !(a:0 && a:1)
        echo 'C/C++ style: ' . l:name
    endif
endfunction

" First FileType only. Skip if the user already ran :MICROStyle etc.
function! s:AutoCStyle() abort
    if get(b:, 'micro_c_style', '') !=# ''
        return
    endif
    let l:path = expand('%:p')
    if l:path =~# '/zircon/'
        call s:ApplyCStyle('hypervisor', 1)
    elseif l:path =~# '/userspace/'
        call s:ApplyCStyle('google', 1)
    endif
endfunction

if exists(':BEAUStyle') == 2
    delcommand BEAUStyle
endif
command! MICROStyle call <SID>ApplyCStyle('hypervisor')
command! GOOGLEStyle call <SID>ApplyCStyle('google')
command! LINUXStyle call <SID>ApplyCStyle('linux')

" --- File defaults -----------------------------------------------------------
" formatprg feeds :Format / Space-=. gn/fidl get commentstring so Space-/
" works. Last-position restore skips special buffers. checktime implements
" autoread when focus returns.
function! s:ApplyCodeFormatOptions() abort
    if get(b:, 'micro_large_file', 0) || &buftype !=# ''
        return
    endif
    if index(['gitcommit', 'gitrebase', 'markdown', 'text', 'mail'], &filetype) >= 0
        return
    endif
    setlocal formatoptions+=j
    setlocal formatoptions+=rql
    setlocal formatoptions-=t
    setlocal formatoptions-=o
endfunction

augroup micro_file_defaults
    autocmd!
    autocmd FileType * call <SID>ApplyCodeFormatOptions()
    autocmd FileType c,cpp,objc,objcpp
        \ if executable('clang-format') |
        \     setlocal formatprg=clang-format |
        \ endif |
        \ setlocal suffixesadd=.h,.hh,.hpp,.hxx,.c,.cc,.cpp,.cxx,.inl,.inc,.S,.s |
        \ call <SID>AutoCStyle()
    autocmd FileType rust
        \ if executable('rustfmt') |
        \     setlocal formatprg=rustfmt |
        \ endif |
        \ setlocal suffixesadd=.rs
    autocmd FileType asm,gas
        \ setlocal suffixesadd=.h,.S,.s,.c commentstring=//\ %s
    autocmd FileType gn
        \ setlocal commentstring=#\ %s comments=:#
        \ tabstop=2 shiftwidth=2 softtabstop=2 expandtab
    autocmd FileType fidl
        \ setlocal commentstring=//\ %s
        \ tabstop=2 shiftwidth=2 softtabstop=2 expandtab
    autocmd BufNewFile,BufRead *.gn,*.gni set filetype=gn
    autocmd BufNewFile,BufRead *.fidl set filetype=fidl
    autocmd BufReadPost *
        \ if !get(b:, 'micro_large_file', 0)
        \     && line("'\"") > 0 && line("'\"") <= line('$') && &buftype ==# '' |
        \     execute 'normal! g`"' |
        \ endif
    autocmd FocusGained,BufEnter * silent! checktime
augroup END

" --- Large / AEE dump files --------------------------------------------------
" *_RAW (and mini-rdump / oversized files) are often one 4-6 MiB line with
" no newlines. wrap + linebreak + list + syntax walk every byte and freeze.
" Cheap options must be set in BufReadPre, before the buffer is filled.
" clipboard/regexpengine/hlsearch are global: snapshot on enter, restore
" on leave so / * yank do not scan the whole dump, then other buffers
" keep unnamedplus.
" Restore eventignore if :source interrupted a dump BufReadPre/Post pair.
if exists('s:large_file_ei_depth') && s:large_file_ei_depth > 0
        \ && exists('s:large_file_ei_saved')
    let &eventignore = s:large_file_ei_saved
endif
let s:large_file_ei_depth = 0
let s:large_file_ei_saved = ''
if exists('s:large_file_globals')
    let &clipboard = s:large_file_globals.clipboard
    let &regexpengine = s:large_file_globals.regexpengine
    let &hlsearch = s:large_file_globals.hlsearch
    let &incsearch = s:large_file_globals.incsearch
    unlet s:large_file_globals
endif

function! s:IsDumpOrLargePath(path) abort
    if empty(a:path)
        return 0
    endif
    let l:name = fnamemodify(a:path, ':t')
    if l:name =~# '_RAW$' || l:name =~# '_RAW\.'
        return 1
    endif
    if l:name =~# 'MINI_RDUMP$' || l:name =~# '\.dfd$'
        return 1
    endif
    " Name-based dumps are caught above. Size is a last-resort Pre gate for
    " unnamed huge binaries; text logs with newlines are handled in Post.
    let l:size = getfsize(a:path)
    return l:size >= 8 * 1024 * 1024
endfunction

function! s:IsDumpTreePath(path) abort
    return a:path =~# '\.dbg\.DEC[/\\]\|[/\\]aee_exp[/\\]'
endfunction

function! s:LargeFileIgnoreFileType() abort
    if s:large_file_ei_depth == 0
        let s:large_file_ei_saved = &eventignore
    endif
    let s:large_file_ei_depth += 1
    if &eventignore !~# 'FileType'
        set eventignore+=FileType
    endif
endfunction

function! s:LargeFileRestoreFileType() abort
    if s:large_file_ei_depth <= 0
        return
    endif
    let s:large_file_ei_depth -= 1
    if s:large_file_ei_depth == 0
        let &eventignore = s:large_file_ei_saved
    endif
endfunction

function! s:ApplyLargeFileOptions() abort
    setlocal binary noeol nomodeline
    setlocal noswapfile noundofile
    setlocal undolevels=-1
    setlocal nowrap nolinebreak nobreakindent
    setlocal nolist nocursorline norelativenumber
    if exists('+signcolumn')
        setlocal signcolumn=no
    endif
    if exists('+smoothscroll')
        setlocal nosmoothscroll
    endif
    setlocal synmaxcol=256
    setlocal matchpairs=
    setlocal foldmethod=manual
    setlocal nofoldenable
    setlocal display+=uhex
    let b:matchparen_disable = 1
    let b:no_matchparen = 1
endfunction

" Many-line text dumps (SYS_KERNEL_LOG): keep syntax, drop wrap/linebreak.
function! s:ApplyLongTextOptions() abort
    setlocal nowrap nolinebreak nobreakindent
    setlocal norelativenumber
    if exists('+signcolumn')
        setlocal signcolumn=no
    endif
    if exists('+smoothscroll')
        setlocal nosmoothscroll
    endif
    setlocal synmaxcol=256
    let b:micro_long_text = 1
endfunction

function! s:LargeFileDisplayOnly() abort
    setlocal nowrap nolinebreak nobreakindent
    setlocal nolist nocursorline norelativenumber
    if exists('+signcolumn')
        setlocal signcolumn=no
    endif
    if exists('+smoothscroll')
        setlocal nosmoothscroll
    endif
endfunction

" regexpengine/clipboard/hlsearch are global. Apply only while a dump is
" the current buffer so a split with source code is not stuck on engine=1.
function! s:SyncLargeFileSession() abort
    let l:want = get(b:, 'micro_large_file', 0)
    let l:active = exists('s:large_file_globals')
    if l:want && !l:active
        let s:large_file_globals = {
            \ 'clipboard': &clipboard,
            \ 'regexpengine': &regexpengine,
            \ 'hlsearch': &hlsearch,
            \ 'incsearch': &incsearch
            \ }
        set clipboard=
        set regexpengine=1
        set nohlsearch
        set noincsearch
        let v:hlsearch = 0
    elseif !l:want && l:active
        let &clipboard = s:large_file_globals.clipboard
        let &regexpengine = s:large_file_globals.regexpengine
        let &hlsearch = s:large_file_globals.hlsearch
        let &incsearch = s:large_file_globals.incsearch
        unlet s:large_file_globals
    endif
endfunction

function! s:LargeFilePre() abort
    let l:path = expand('<afile>:p')
    if !s:IsDumpOrLargePath(l:path)
        return
    endif
    let b:micro_large_file = 1
    call s:LargeFileIgnoreFileType()
    call s:ApplyLargeFileOptions()
endfunction

function! s:LargeFilePost() abort
    if !get(b:, 'micro_large_file', 0)
        let l:huge = 0
        let l:last = min([20, line('$')])
        let l:lnum = 1
        while l:lnum <= l:last
            if col([l:lnum, '$']) > 8192
                let l:huge = 1
                break
            endif
            let l:lnum += 1
        endwhile
        if !l:huge
            if line('$') > 10000
                call s:ApplyLongTextOptions()
            endif
            return
        endif
        let b:micro_large_file = 1
        call s:ApplyLargeFileOptions()
    else
        call s:LargeFileRestoreFileType()
        call s:ApplyLargeFileOptions()
    endif
    silent! syntax clear
    setlocal filetype=
    call s:SyncLargeFileSession()
endfunction

function! s:LargeFileWin() abort
    if get(b:, 'micro_large_file', 0)
        call s:LargeFileDisplayOnly()
        call s:SyncLargeFileSession()
    elseif get(b:, 'micro_long_text', 0)
        setlocal nowrap nolinebreak nobreakindent
        setlocal norelativenumber
        if exists('+signcolumn')
            setlocal signcolumn=no
        endif
        if exists('+smoothscroll')
            setlocal nosmoothscroll
        endif
        call s:SyncLargeFileSession()
    else
        if exists('+signcolumn') && &number
            setlocal signcolumn=yes
        endif
        call s:SyncLargeFileSession()
    endif
endfunction

augroup micro_large_files
    autocmd!
    autocmd BufReadPre * call <SID>LargeFilePre()
    autocmd BufReadPost * call <SID>LargeFilePost()
    autocmd BufEnter,BufWinEnter * call <SID>LargeFileWin()
augroup END

" --- Clipboard ---------------------------------------------------------------
" System clipboard. Prefer X11/Wayland when a display exists.
" Over SSH/tty, force OSC 52 before unnamedplus so yank does not emit W23.
" Skip OSC 52 on linux/cons/dumb/vt terminals to avoid escape garbage.
" Disable OSC paste so a terminal without it cannot block on paste.
if has('clipboard_provider') && exists('+clipmethod')
    let g:osc52_disable_paste = 1
    try
        " No native display: do not wait for DA1 (unreliable over SSH/tmux).
        " Skip linux/cons/dumb/vt so OSC 52 escapes are not printed as text.
        if empty($DISPLAY) && empty($WAYLAND_DISPLAY)
            \ && &term !~? '^\%(linux\|cons\|dumb\|vt\)'
            let g:osc52_force_avail = 1
            packadd osc52
            set clipmethod=osc52
        else
            packadd osc52
            if &clipmethod !~# '\<osc52\>'
                let &clipmethod .= (empty(&clipmethod) ? '' : ',') . 'osc52'
            endif
        endif
    catch
    endtry
endif
" unnamedplus maps yank/delete to + and emits W23 when v:clipmethod is none.
if exists('v:clipmethod') && v:clipmethod !=# 'none'
    set clipboard=unnamedplus
endif
set linespace=0

" --- Display, search, files --------------------------------------------------
" Render tabs, trailing spaces, and off-screen overflow markers
set list
set listchars=tab:·\ ,trail:·,nbsp:␣,precedes:←,extends:→
" Soft wrapping for long lines
set wrap
set linebreak
set breakindent
set breakindentopt=shift:2
" Command feedback, searching, and status display.
" noshowmode/noruler: the custom statusline already shows mode and position.
" showcmdloc=statusline: pending operator keys appear in %S, not the cmdline.
set laststatus=2
set cmdheight=1
set noshowmode
set showcmd
if exists('+showcmdloc')
    set showcmdloc=statusline
endif
set noruler
set hlsearch
set incsearch
set ignorecase
set smartcase
set infercase
set showmatch
set matchtime=1
set nobackup
set shortmess+=c
set shortmess-=S
set formatoptions+=j
set formatoptions+=rql
set formatoptions-=t
set formatoptions-=o
set synmaxcol=1000
" No swap files. Crash recovery is via undofile under ~/.vim/undo instead.
" updatecount=0 also blocks a later :set swapfile from creating .swp files.
set noswapfile
set updatecount=0
let s:undo_dir = expand('~/.vim/undo')
call mkdir(s:undo_dir, 'p', 0700)
let &undodir = s:undo_dir . '//'
set undofile
set guioptions-=T
set guioptions-=m
set report=0
set cursorline
" Popup completion for Ex commands and file paths (for example, :write)
set wildmenu
set wildmode=longest:full,full
set wildoptions=pum
silent! set wildoptions+=fuzzy
set wildignorecase
set wildignore+=*/.git/*,*/.hg/*,*/.svn/*
set wildignore+=*/out/*,*/build/*,*/target/*,*/node_modules/*
set wildignore+=*.o,*.obj,*.a,*.so,*.pyc,*.class,*.swp,*.bin
set wildignore+=*.d,*.gcda,*.gcno
set wildignore+=tags,TAGS,GTAGS,GRTAGS,GPATH,cscope.out
" Skip included-file scans in completion; they hitch on Zircon-sized trees.
set complete-=i
set pumheight=12
set completeopt=menuone,popup
silent! set completeopt+=fuzzy
if exists('+completepopup')
    set completepopup=height:12,width:70,border:round,highlight:Pmenu
endif
if exists('+previewpopup')
    set previewpopup=height:20,width:80
endif
" Hide the default split separator for a cleaner layout
let &fillchars = 'vert: ,eob: '
set diffopt=internal,filler,closeoff,hiddenoff,algorithm:histogram,indent-heuristic,linematch:60
set backspace=indent,eol,start
" Search tags from the current file upward, then from the working directory.
set tags=./tags;,tags
if exists('+tagcase')
    set tagcase=followscs
endif

" :grep and fzf file listing share the same hidden-but-not-.git rg flags.
" Shared rg globs: skip git internals and AEE dump binaries (preview/open
" of *_RAW in fzf would hitch even though Vim itself now handles them).
let s:rg_exclude_globs = '--glob=!.git/* --glob=!*_RAW --glob=!*MINI_RDUMP --glob=!*.dfd'
if executable('rg')
    let &grepprg = 'rg --vimgrep --smart-case --hidden ' . s:rg_exclude_globs
    set grepformat=%f:%l:%c:%m
    if empty($FZF_DEFAULT_COMMAND)
        let $FZF_DEFAULT_COMMAND = 'rg --files --hidden ' . s:rg_exclude_globs
    endif
endif

let g:hlyank_hlgroup = 'IncSearch'
let g:hlyank_duration = 250
let g:EditorConfig_exclude_patterns = ['fugitive://.*', 'scp://.*', '.*_RAW', '.*MINI_RDUMP', '.*\.dfd']

" --- Statusline --------------------------------------------------------------
" Layout: MICRO:<mode>  <path>  [branch]  flags  %S  |  <ft> <pos>
" MicroStatusMode/MicroGitBranch are global so %{...} in 'statusline' can call them.
function! MicroStatusMode() abort
    let l:mode = mode(1)
    if l:mode =~# '^n'
        return 'NORMAL'
    elseif l:mode =~# '^i'
        return 'INSERT'
    elseif l:mode =~# '^R'
        return 'REPLACE'
    elseif l:mode ==# 'v'
        return 'VISUAL'
    elseif l:mode ==# 'V'
        return 'V-LINE'
    elseif l:mode ==# "\<C-v>"
        return 'V-BLOCK'
    elseif l:mode =~# '^c'
        return 'COMMAND'
    elseif l:mode =~# '^t'
        return 'TERMINAL'
    endif
    return toupper(l:mode)
endfunction

" Filesystem git lookup; no process spawn. Cached per starting directory.
" Also used by git signs. Walks parents until .git is a dir or a gitfile
" (worktrees / submodules). Empty string means "not in a repository".
let s:git_root_cache = {}
let s:git_dir_cache = {}

function! s:GitRoot(path) abort
    if empty(a:path)
        return ''
    endif
    let l:dir = fnamemodify(a:path, ':p:h')
    if has_key(s:git_root_cache, l:dir)
        return s:git_root_cache[l:dir]
    endif

    let l:cur = l:dir
    let l:root = ''
    while 1
        if isdirectory(l:cur . '/.git') || filereadable(l:cur . '/.git')
            let l:root = l:cur
            break
        endif
        let l:parent = fnamemodify(l:cur, ':h')
        if l:parent ==# l:cur
            break
        endif
        let l:cur = l:parent
    endwhile
    let s:git_root_cache[l:dir] = l:root
    return l:root
endfunction

" Resolve $GIT_DIR. Regular repos use <root>/.git/; worktrees store
" "gitdir: <path>" in that file.
function! s:GitDir(root) abort
    if empty(a:root)
        return ''
    endif
    if has_key(s:git_dir_cache, a:root)
        return s:git_dir_cache[a:root]
    endif

    let l:git = a:root . '/.git'
    let l:dir = ''
    if isdirectory(l:git)
        let l:dir = l:git
    elseif filereadable(l:git)
        let l:line = get(readfile(l:git, '', 1), 0, '')
        let l:gitdir = matchstr(l:line, '^gitdir:\s*\zs.*')
        if !empty(l:gitdir)
            if l:gitdir !~# '^[/\\]\|^\a:'
                let l:gitdir = a:root . '/' . l:gitdir
            endif
            let l:dir = substitute(fnamemodify(l:gitdir, ':p'), '[/\\]$', '', '')
        endif
    endif
    let s:git_dir_cache[a:root] = l:dir
    return l:dir
endfunction

" Cheap invalidation key: HEAD or index mtime changed => refresh branch/signs.
function! s:GitStateStamp(root) abort
    let l:gitdir = s:GitDir(a:root)
    if empty(l:gitdir)
        return ''
    endif
    return getftime(l:gitdir . '/HEAD') . '/' . getftime(l:gitdir . '/index')
endfunction

" Path relative to the repo root, as git ls-files / git diff expect.
function! s:GitRelativePath(root, path) abort
    let l:root = substitute(fnamemodify(a:root, ':p'), '[/\\]$', '', '')
    let l:path = fnamemodify(a:path, ':p')
    if l:path ==# l:root
        return '.'
    endif
    if strpart(l:path, 0, strlen(l:root) + 1) !=# l:root . '/'
        return ''
    endif
    return strpart(l:path, strlen(l:root) + 1)
endfunction

" Reload-safe: kill in-flight git symbolic-ref jobs from a previous :source.
if exists('s:git_branch_jobs')
    for s:git_branch_job in values(s:git_branch_jobs)
        try
            call job_stop(s:git_branch_job, 'kill')
        catch
        endtry
    endfor
endif
let s:git_branch_jobs = {}
if !exists('s:git_branch_by_root')
    let s:git_branch_by_root = {}
endif
if !exists('s:git_branch_poll')
    let s:git_branch_poll = {}
endif
if !exists('s:git_branch_stamp')
    let s:git_branch_stamp = {}
endif

function! s:GitBranchOut(root, channel, msg) abort
    let s:git_branch_by_root[a:root] = trim(a:msg)
    redrawstatus
endfunction

function! s:GitBranchExit(root, job, status) abort
    if has_key(s:git_branch_jobs, a:root)
        call remove(s:git_branch_jobs, a:root)
    endif
    if !has_key(s:git_branch_by_root, a:root)
        let s:git_branch_by_root[a:root] = ''
    endif
    let s:git_branch_stamp[a:root] = s:GitStateStamp(a:root)
    redrawstatus
endfunction

" One job per repo root. Detached HEAD falls back to the short commit hash.
function! s:RefreshGitBranchAsync(root) abort
    if !has('job') || !executable('git') || empty(a:root)
        return
    endif
    if has_key(s:git_branch_jobs, a:root)
        return
    endif

    let l:cmd = 'git -C ' . shellescape(a:root)
        \ . ' symbolic-ref --quiet --short HEAD 2>/dev/null'
        \ . ' || git -C ' . shellescape(a:root)
        \ . ' rev-parse --short HEAD 2>/dev/null'
    let s:git_branch_jobs[a:root] = job_start(['sh', '-c', l:cmd], {
        \ 'in_io': 'null',
        \ 'err_io': 'null',
        \ 'out_mode': 'nl',
        \ 'out_cb': function('<SID>GitBranchOut', [a:root]),
        \ 'exit_cb': function('<SID>GitBranchExit', [a:root])
        \ })
endfunction

" Statusline callback: return cached branch text, and start a refresh if
" HEAD/index changed. Must stay cheap; it runs on every statusline redraw
" (showcmdloc=statusline). Stat HEAD/index at most once per second.
function! MicroGitBranch() abort
    if &buftype !=# '' || empty(expand('%:p'))
        return ''
    endif
    if get(b:, 'micro_large_file', 0)
        return ''
    endif

    let l:root = s:GitRoot(expand('%:p'))
    if empty(l:root)
        return ''
    endif

    let l:now = reltimefloat(reltime())
    let l:prev = get(s:git_branch_poll, l:root, [0.0, ''])
    if l:now - l:prev[0] >= 1.0
        let l:stamp = s:GitStateStamp(l:root)
        let s:git_branch_poll[l:root] = [l:now, l:stamp]
    else
        let l:stamp = l:prev[1]
    endif

    if get(s:git_branch_stamp, l:root, '') !=# l:stamp
            \ || !has_key(s:git_branch_by_root, l:root)
        call s:RefreshGitBranchAsync(l:root)
    endif

    let l:branch = get(s:git_branch_by_root, l:root, '')
    return empty(l:branch) ? '' : ' 🅑  ' . l:branch
endfunction

function! s:ApplyStatuslineColors() abort
    highlight MicroStatusMode cterm=bold ctermfg=0 ctermbg=12
        \ gui=bold guifg=#1a1b26 guibg=#7aa2f7
    highlight MicroStatusFile cterm=NONE ctermfg=15 ctermbg=8
        \ gui=NONE guifg=#c0caf5 guibg=#32344a
    highlight MicroStatusGit cterm=bold ctermfg=10 ctermbg=8
        \ gui=bold guifg=#9ece6a guibg=#32344a
    highlight MicroStatusMeta cterm=bold ctermfg=0 ctermbg=12
        \ gui=bold guifg=#1a1b26 guibg=#7aa2f7
endfunction

call s:ApplyStatuslineColors()

augroup micro_statusline_colors
    autocmd!
    autocmd ColorScheme * call <SID>ApplyStatuslineColors()
augroup END

augroup micro_git_branch_status
    autocmd!
    autocmd DirChanged,FocusGained * redrawstatus
augroup END

" %S is the pending showcmd (operators/counts) because showcmdloc=statusline.
set statusline=
let &statusline .= '%#MicroStatusMode# MICRO:%{MicroStatusMode()} '
set statusline+=%#MicroStatusFile#\ %<%F
let &statusline .= '%#MicroStatusGit#%{MicroGitBranch()}'
set statusline+=\ %h%m%r%w\ %S
set statusline+=%=
let &statusline .= '🅡  %#MicroStatusMeta# %y %l:%c %p%% '

" --- Command-line UI ---------------------------------------------------------
" Native ':' command line. Idle command bar shows EVERYDAY'S A HUSTLE
" with no highlight. SafeState keeps it there; cheaper than remapping
" ':' to input().
" Temporarily link MsgArea to Pmenu and hide showcmd so the panel is uncluttered.
silent! nunmap :
silent! xunmap :
let s:cmdline_msgarea = []
let s:cmdline_showcmd = &showcmd
let s:cmdline_prompt = "EVERYDAY'S A HUSTLE"

function! s:ShowCmdPrompt() abort
    let l:m = mode()
    if l:m =~# '^c' || l:m ==# 'r' || l:m ==# 'rm'
        return
    endif
    echo s:cmdline_prompt
endfunction

function! s:CmdlineUiEnter() abort
    if !exists('*hlget') || !exists('*hlset')
        return
    endif
    let s:cmdline_msgarea = hlget('MsgArea')
    let s:cmdline_showcmd = &showcmd
    set noshowcmd
    call hlset([{'name': 'MsgArea', 'linksto': 'Pmenu'}])
    redraw
endfunction

function! s:CmdlineUiLeave() abort
    if exists('*hlset') && !empty(s:cmdline_msgarea)
        call hlset(s:cmdline_msgarea)
    endif
    let &showcmd = s:cmdline_showcmd
    call s:ShowCmdPrompt()
endfunction

augroup micro_cmdline_ui
    autocmd!
    autocmd CmdlineEnter : call <SID>CmdlineUiEnter()
    autocmd CmdlineLeave : call <SID>CmdlineUiLeave()
    autocmd VimEnter,SafeState * call <SID>ShowCmdPrompt()
augroup END

" --- Floating terminal -------------------------------------------------------
" Reusable floating shell. buf holds the job; win is the popup.
" Hide (Space-t / Ctrl-Q) keeps the process and cwd; Esc Esc wipes it.
" exists() keeps the job alive across :source $MYVIMRC.
if !exists('s:floating_terminal')
    let s:floating_terminal = {'buf': -1, 'win': -1}
endif

function! s:ApplyFloatingTerminalColors() abort
    highlight MicroTerminal ctermfg=15 ctermbg=NONE
        \ guifg=#c0caf5 guibg=NONE
    highlight MicroTerminalBorder cterm=bold ctermfg=12 ctermbg=NONE
        \ gui=bold guifg=#7aa2f7 guibg=NONE
    if has('popupwin') && get(s:floating_terminal, 'win', -1) > 0
            \ && !empty(popup_getpos(s:floating_terminal.win))
        call popup_setoptions(s:floating_terminal.win, {
            \ 'highlight': 'MicroTerminal',
            \ 'borderhighlight': ['MicroTerminalBorder']
            \ })
    endif
endfunction

function! s:FloatingTerminalSize() abort
    let l:width = max([20, min([float2nr(&columns * 0.85), &columns - 4])])
    let l:height = max([5, min([float2nr((&lines - 2) * 0.70), &lines - 4])])
    return {
        \ 'minwidth': l:width,
        \ 'maxwidth': l:width,
        \ 'minheight': l:height,
        \ 'maxheight': l:height
        \ }
endfunction

function! s:FloatingTerminalVisible() abort
    if s:floating_terminal.win <= 0
        return 0
    endif
    let l:pos = popup_getpos(s:floating_terminal.win)
    return !empty(l:pos) && get(l:pos, 'visible', 0)
endfunction

function! s:FloatingTerminalAlive() abort
    if s:floating_terminal.buf <= 0
        \ || !bufexists(s:floating_terminal.buf)
        return 0
    endif
    try
        return job_status(term_getjob(s:floating_terminal.buf)) ==# 'run'
    catch
        return 0
    endtry
endfunction

function! s:FloatingTerminalClosed(id, result) abort
    if s:floating_terminal.win == a:id
        let s:floating_terminal.win = -1
    endif
endfunction

function! s:CloseFloatingTerminal() abort
    let l:buf = s:floating_terminal.buf
    let l:win = s:floating_terminal.win
    let s:floating_terminal = {'buf': -1, 'win': -1}

    if l:win > 0 && !empty(popup_getpos(l:win))
        call popup_close(l:win)
    endif
    if l:buf > 0 && bufexists(l:buf)
        execute 'silent! bwipeout! ' . l:buf
    endif
endfunction

function! s:ResizeFloatingTerminal() abort
    if s:FloatingTerminalVisible()
        call popup_setoptions(
            \ s:floating_terminal.win, s:FloatingTerminalSize())
    endif
endfunction

" popup_create leaves Terminal-Normal; feed 'i' so the shell accepts keys.
function! s:EnsureFloatingTerminalJobMode(timer) abort
    if s:FloatingTerminalVisible() && mode(1) !~# '^t'
        call feedkeys('i', 'n')
    endif
endfunction

function! s:ConfigureFloatingTerminalKeys() abort
    " Double Esc closes the shell; Ctrl-Q only hides it for later reuse.
    call win_execute(s:floating_terminal.win,
        \ 'tnoremap <silent> <buffer> <Esc><Esc> '
        \ . '<C-\><C-N><Cmd>FloatTerminalClose<CR>')
    call win_execute(s:floating_terminal.win,
        \ 'tnoremap <silent> <buffer> <C-q> '
        \ . '<C-\><C-N><Cmd>FloatTerminal<CR>')
    " Keep mouse-wheel scrolling without showing a popup scrollbar.
    call win_execute(s:floating_terminal.win,
        \ 'tnoremap <silent> <buffer> <ScrollWheelUp> '
        \ . '<C-\><C-N>3k')
    call win_execute(s:floating_terminal.win,
        \ 'tnoremap <silent> <buffer> <ScrollWheelDown> '
        \ . '<C-\><C-N>3j')
    call win_execute(s:floating_terminal.win,
        \ 'tnoremap <silent> <buffer> <S-ScrollWheelUp> '
        \ . '<C-\><C-N>15k')
    call win_execute(s:floating_terminal.win,
        \ 'tnoremap <silent> <buffer> <S-ScrollWheelDown> '
        \ . '<C-\><C-N>15j')
    " Keep handling the wheel while the terminal is in Terminal-Normal mode.
    call win_execute(s:floating_terminal.win,
        \ 'nnoremap <silent> <buffer> <ScrollWheelUp> 3k')
    call win_execute(s:floating_terminal.win,
        \ 'nnoremap <silent> <buffer> <ScrollWheelDown> 3j')
    call win_execute(s:floating_terminal.win,
        \ 'nnoremap <silent> <buffer> <S-ScrollWheelUp> 15k')
    call win_execute(s:floating_terminal.win,
        \ 'nnoremap <silent> <buffer> <S-ScrollWheelDown> 15j')
endfunction

" Visible => hide popup. Dead job => start a new shell. Else reuse buf.
function! s:ToggleFloatingTerminal() abort
    if !has('popupwin') || !has('terminal')
        echoerr 'Floating terminal requires +popupwin and +terminal'
        return
    endif
    if s:FloatingTerminalVisible()
        call popup_close(s:floating_terminal.win)
        let s:floating_terminal.win = -1
        return
    endif

    let l:size = s:FloatingTerminalSize()
    if !s:FloatingTerminalAlive()
        if s:floating_terminal.buf > 0
            \ && bufexists(s:floating_terminal.buf)
            execute 'silent! bwipeout! ' . s:floating_terminal.buf
        endif
        let s:floating_terminal.buf = term_start(&shell, {
            \ 'hidden': 1,
            \ 'term_finish': 'close',
            \ 'term_name': ' 🅣  HUSTLE ',
            \ 'term_highlight': 'MicroTerminal',
            \ 'term_rows': l:size.minheight,
            \ 'term_cols': l:size.minwidth
            \ })
        if s:floating_terminal.buf <= 0
            echoerr 'Unable to start floating terminal'
            return
        endif
        call setbufvar(s:floating_terminal.buf, '&bufhidden', 'hide')
        " Keep enough terminal history for mouse-wheel review.
        call setbufvar(s:floating_terminal.buf, '&termwinscroll', 2000)
        call term_setkill(s:floating_terminal.buf, 'kill')
    endif

    let l:options = extend(l:size, {
        \ 'pos': 'center',
        \ 'border': [1, 1, 1, 1],
        \ 'borderchars': ['─', '│', '─', '│', '┌', '┐', '┘', '└'],
        \ 'padding': [0, 1, 0, 1],
        \ 'title': ' 🅣  HUSTLE ',
        \ 'close': 'none',
        \ 'drag': 1,
        \ 'resize': 1,
        \ 'scrollbar': 0,
        \ 'highlight': 'MicroTerminal',
        \ 'borderhighlight': ['MicroTerminalBorder'],
        \ 'callback': function('<SID>FloatingTerminalClosed')
        \ })
    try
        let s:floating_terminal.win = popup_create(
            \ s:floating_terminal.buf, l:options)
        call s:ConfigureFloatingTerminalKeys()
        call timer_start(0, function('<SID>EnsureFloatingTerminalJobMode'))
    catch
        let s:floating_terminal.win = -1
        echohl ErrorMsg
        echomsg substitute(v:exception, '^Vim\%((\a\+)\)\=:', '', '')
        echohl None
    endtry
endfunction

call s:ApplyFloatingTerminalColors()
command! FloatTerminal call <SID>ToggleFloatingTerminal()
command! FloatTerminalClose call <SID>CloseFloatingTerminal()

augroup micro_floating_terminal
    autocmd!
    autocmd ColorScheme * call <SID>ApplyFloatingTerminalColors()
    autocmd VimResized * call <SID>ResizeFloatingTerminal()
augroup END

" --- fzf / live ripgrep ------------------------------------------------------
" ~/.fzf is the fzf binary install; fzf.vim is a pack plugin under ~/.vim.
set rtp+=~/.fzf

let g:fzf_layout = {
    \ 'window': {
    \   'width': 0.92,
    \   'height': 0.85,
    \   'highlight': 'Normal',
    \   'border': 'rounded'
    \ }
    \ }
let g:fzf_vim = {}
let g:fzf_vim.preview_window = ['right,55%,<100(up,45%)', 'ctrl-/']
let g:fzf_vim.buffers_jump = 1
" Space-fl / :BLines: exact substrings only, not fuzzy character matching.
let g:fzf_vim.blines_options = ['--exact']

function! s:ConfigureFzfTerminalKeys() abort
    " Keep repeated Esc inside fzf instead of entering Terminal-Normal mode.
    tnoremap <silent> <buffer> <Esc><Esc> <Esc>
    tnoremap <silent> <buffer> :q<CR> <Esc>
endfunction

augroup micro_fzf_terminal_keys
    autocmd!
    autocmd FileType fzf call <SID>ConfigureFzfTerminalKeys()
augroup END

let s:rg_history = {
    \ 'text': expand('~/.vim/rg-history'),
    \ 'regex': expand('~/.vim/rg-regex-history')
    \ }

" All Files/Rg variants stay anchored to the directory where Vim started,
" even after :cd or NERDTree. exists() preserves the root across :source.
if !exists('g:micro_workspace_root')
    let g:micro_workspace_root = getcwd()
endif

function! s:FzfVimAvailable() abort
    return get(g:, 'loaded_fzf_vim', 0) || exists(':Files') == 2
endfunction

" Resolve a:sub to a directory under the workspace. Empty => workspace root.
function! s:ResolveRgDir(sub) abort
    let l:root = substitute(fnamemodify(g:micro_workspace_root, ':p'), '/\+$', '', '')
    let l:sub = substitute(trim(a:sub), '/\+$', '', '')
    if empty(l:sub) || l:sub ==# '.'
        return l:root
    endif

    if l:sub =~# '^/'
        let l:dir = fnamemodify(l:sub, ':p')
    else
        let l:ws = fnamemodify(l:root . '/' . l:sub, ':p')
        let l:dir = isdirectory(l:ws) ? l:ws : fnamemodify(l:sub, ':p')
    endif
    let l:dir = substitute(l:dir, '/\+$', '', '')

    if !isdirectory(l:dir)
        echo 'Not a directory: ' . l:sub
        return ''
    endif
    if l:dir !=# l:root && stridx(l:dir . '/', l:root . '/') != 0
        echo 'Dir is outside workspace: ' . l:dir
        return ''
    endif
    return l:dir
endfunction

" Space-fw: narrow float for the search subdirectory, then live rg.
if exists('s:fw_popup')
    if get(s:fw_popup, 'win', 0) > 0
        silent! call popup_close(s:fw_popup.win)
    endif
endif
let s:fw_popup = {'win': -1, 'dir': '', 'word': ''}

function! s:FwDirCandidates(typed) abort
    let l:root = substitute(fnamemodify(g:micro_workspace_root, ':p'), '/\+$', '', '')
    if a:typed =~# '/$'
        let l:parent = substitute(a:typed, '/$', '', '')
        let l:pat = l:root . '/' . l:parent . '/*'
        let l:prefix = l:parent . '/'
    elseif a:typed =~# '/'
        let l:parent = substitute(a:typed, '/[^/]*$', '', '')
        let l:base = matchstr(a:typed, '[^/]*$')
        let l:pat = l:root . '/' . l:parent . '/' . l:base . '*'
        let l:prefix = l:parent . '/'
    else
        let l:pat = l:root . '/' . a:typed . '*'
        let l:prefix = ''
    endif
    let l:cands = []
    for l:path in glob(l:pat, 1, 1)
        if isdirectory(l:path)
            call add(l:cands, l:prefix . fnamemodify(l:path, ':t'))
        endif
    endfor
    return sort(l:cands)
endfunction

function! s:FwCommonPrefix(items) abort
    if empty(a:items)
        return ''
    endif
    let l:pfx = a:items[0]
    for l:item in a:items[1:]
        let l:n = min([strlen(l:pfx), strlen(l:item)])
        let l:i = 0
        while l:i < l:n && l:pfx[l:i] ==# l:item[l:i]
            let l:i += 1
        endwhile
        let l:pfx = strpart(l:pfx, 0, l:i)
        if empty(l:pfx)
            return ''
        endif
    endfor
    return l:pfx
endfunction

" Tab: fill unique dir, else longest common prefix. No match list.
function! s:FwCompleteDir() abort
    let l:typed = s:fw_popup.dir
    let l:cands = s:FwDirCandidates(l:typed)
    if empty(l:cands)
        return
    endif
    if len(l:cands) == 1
        let s:fw_popup.dir = l:cands[0] . '/'
        return
    endif
    let l:lcp = s:FwCommonPrefix(l:cands)
    if !empty(l:lcp) && stridx(l:lcp, l:typed) == 0 && l:lcp !=# l:typed
        let s:fw_popup.dir = l:lcp
    endif
endfunction

function! s:FwWidth() abort
    return max([20, min([float2nr(&columns * 0.85), &columns - 4])])
endfunction

function! s:FwRender() abort
    let l:id = s:fw_popup.win
    if l:id <= 0
        return
    endif
    call popup_settext(l:id, [
        \ 'press ENTER = workspace  Tab complete',
        \ '> ' . s:fw_popup.dir . '█'
        \ ])
endfunction

function! s:FwFilter(id, key) abort
    if a:key ==# "\<Esc>" || a:key ==# "\<C-c>"
        call popup_close(a:id, 0)
        return 1
    endif
    if a:key ==# "\<CR>" || a:key ==# "\<C-m>"
        call popup_close(a:id, {
            \ 'dir': s:fw_popup.dir,
            \ 'word': s:fw_popup.word
            \ })
        return 1
    endif
    if a:key ==# "\<Tab>"
        call s:FwCompleteDir()
        call s:FwRender()
        return 1
    endif
    if a:key ==# "\<BS>" || a:key ==# "\<C-h>"
        if !empty(s:fw_popup.dir)
            let s:fw_popup.dir = strcharpart(
                \ s:fw_popup.dir, 0, strchars(s:fw_popup.dir) - 1)
            call s:FwRender()
        endif
        return 1
    endif
    if a:key ==# "\<C-u>"
        let s:fw_popup.dir = ''
        call s:FwRender()
        return 1
    endif
    if a:key ==# "\<C-w>"
        let s:fw_popup.dir = substitute(s:fw_popup.dir, '[^/]*/\=$', '', '')
        call s:FwRender()
        return 1
    endif
    if strchars(a:key) == 1 && a:key =~# '^[[:print:]]$'
        let s:fw_popup.dir .= a:key
        call s:FwRender()
        return 1
    endif
    return 1
endfunction

function! s:FwStartSearch(timer) abort
    let l:pending = get(s:fw_popup, 'pending', {})
    if empty(l:pending)
        return
    endif
    let l:dir = s:ResolveRgDir(get(l:pending, 'dir', ''))
    if empty(l:dir)
        return
    endif
    call s:LiveRg(get(l:pending, 'word', ''), 0, 0, l:dir)
endfunction

function! s:FwClosed(id, result) abort
    let s:fw_popup.win = -1
    if type(a:result) != v:t_dict
        return
    endif
    let s:fw_popup.pending = a:result
    call timer_start(0, function('<SID>FwStartSearch'))
endfunction

function! s:OpenFwPopup(word) abort
    if !has('popupwin')
        echoerr 'Word search requires +popupwin'
        return
    endif
    if s:fw_popup.win > 0 && !empty(popup_getpos(s:fw_popup.win))
        call popup_close(s:fw_popup.win, 0)
    endif
    let s:fw_popup.dir = ''
    let s:fw_popup.word = a:word
    let s:fw_popup.pending = {}
    let l:width = s:FwWidth()
    let s:fw_popup.win = popup_create([''], {
        \ 'title': ' 🅢  Search dir ',
        \ 'pos': 'center',
        \ 'minwidth': l:width,
        \ 'maxwidth': l:width,
        \ 'minheight': 2,
        \ 'maxheight': 2,
        \ 'border': [1, 1, 1, 1],
        \ 'borderchars': ['─', '│', '─', '│', '┌', '┐', '┘', '└'],
        \ 'padding': [0, 1, 0, 1],
        \ 'close': 'none',
        \ 'mapping': 0,
        \ 'filter': function('<SID>FwFilter'),
        \ 'callback': function('<SID>FwClosed'),
        \ 'highlight': 'MicroTerminal',
        \ 'borderhighlight': ['MicroTerminalBorder']
        \ })
    call s:FwRender()
endfunction

" a:regex 0 => --fixed-strings (Space-fg). 1 => ripgrep regex (Space-re).
" Optional a:1 is the search root (default: workspace).
function! s:LiveRg(query, fullscreen, regex, ...) abort
    if !executable('rg')
        echoerr 'Rg requires ripgrep (rg) in PATH'
        return
    endif
    if !s:FzfVimAvailable()
        echoerr 'fzf.vim is not available'
        return
    endif

    let l:root = a:0 >= 1 && !empty(a:1) ? a:1 : g:micro_workspace_root
    let l:root = substitute(fnamemodify(l:root, ':p'), '/\+$', '', '')
    let l:flags = '--column --line-number --no-heading --color=always '
        \ . '--smart-case --hidden ' . s:rg_exclude_globs
    if !a:regex
        let l:flags .= ' --fixed-strings'
    endif

    " Do not scan the entire workspace before the first character is typed.
    let l:script = 'test -z "$1" || exec rg ' . l:flags . ' -- "$1"'
    let l:prefix = 'sh -c ' . shellescape(l:script) . ' sh'
    let l:mode = a:regex ? 'Regex' : 'Text'
    let l:history = a:regex ? s:rg_history.regex : s:rg_history.text
    let l:options = [
        \ '--layout=reverse',
        \ '--info=inline-right',
        \ '--history', l:history,
        \ '--prompt', 'Search (' . l:mode . ')> ',
        \ '--header', 'Dir: ' . l:root
        \   . '  |  CTRL-/ preview  CTRL-X/V/T split/vsplit/tab'
        \ ]
    let l:spec = fzf#vim#with_preview({
        \ 'dir': l:root,
        \ 'options': l:options
        \ })
    call fzf#vim#grep2(l:prefix, a:query, l:spec, a:fullscreen)
endfunction

function! s:LiveRgWord() abort
    let l:query = expand('<cword>')
    " Ignore punctuation-only words from filetype-specific iskeyword values.
    if l:query !~# '[[:alnum:]_]'
        let l:query = ''
    endif
    call s:OpenFwPopup(l:query)
endfunction

function! s:LiveRgVisual() abort
    let l:saved = [getreg('z'), getregtype('z')]
    silent normal! gv"zy
    let l:query = trim(substitute(getreg('z'), '\r\?\n', ' ', 'g'))
    call setreg('z', l:saved[0], l:saved[1])
    call s:LiveRg(l:query, 0, 0)
endfunction

" Space-fr: workspace-wide literal replace. Same rg globs as Space-fg.
" Always case-sensitive. Independent find popup, then replace popup, then
" fzf match preview. The last float is Confirm; only that ENTER applies.
if exists('s:fr_popup')
    if get(s:fr_popup, 'win', 0) > 0
        silent! call popup_close(s:fr_popup.win)
    endif
endif
let s:fr_popup = {'win': -1, 'find': '', 'repl': '', 'step': 'find'}

function! s:FrRender() abort
    let l:id = s:fr_popup.win
    if l:id <= 0
        return
    endif
    if s:fr_popup.step ==# 'find'
        call popup_settext(l:id, [
            \ 'ENTER replace  Esc cancel',
            \ '> ' . s:fr_popup.find . '█'
            \ ])
        return
    endif
    if s:fr_popup.step ==# 'repl'
        call popup_settext(l:id, [
            \ 'Find: ' . s:fr_popup.find,
            \ 'ENTER preview  Esc back',
            \ '> ' . s:fr_popup.repl . '█'
            \ ])
        return
    endif
    let l:repl = s:fr_popup.repl
    if empty(l:repl)
        let l:repl = '(empty)'
    endif
    call popup_settext(l:id, [
        \ 'Find:    ' . s:fr_popup.find,
        \ 'Replace: ' . l:repl,
        \ 'ENTER confirm replace all  Esc back'
        \ ])
endfunction

function! s:FrEdit(key) abort
    let l:name = s:fr_popup.step ==# 'find' ? 'find' : 'repl'
    let l:val = s:fr_popup[l:name]
    if a:key ==# "\<BS>" || a:key ==# "\<C-h>"
        if !empty(l:val)
            let s:fr_popup[l:name] = strcharpart(l:val, 0, strchars(l:val) - 1)
        endif
    elseif a:key ==# "\<C-u>"
        let s:fr_popup[l:name] = ''
    elseif a:key ==# "\<C-w>"
        let s:fr_popup[l:name] = substitute(l:val, '\S\+\s*$', '', '')
    elseif strchars(a:key) == 1 && a:key =~# '^[[:print:]]$'
        let s:fr_popup[l:name] .= a:key
    else
        return 0
    endif
    return 1
endfunction

function! s:FrFilter(id, key) abort
    let l:step = s:fr_popup.step
    if a:key ==# "\<C-c>"
        call popup_close(a:id, 0)
        return 1
    endif
    if a:key ==# "\<Esc>"
                \ || (l:step !=# 'find'
                \     && (a:key ==# "\<S-Tab>" || a:key ==# "\<Up>"
                \         || a:key ==# "\<C-p>"))
                \ || (l:step ==# 'confirm' && a:key =~# '^[nN]$')
        if l:step ==# 'find'
            call popup_close(a:id, 0)
        else
            call popup_close(a:id, {'action': 'back'})
        endif
        return 1
    endif
    if a:key ==# "\<CR>" || a:key ==# "\<C-m>"
                \ || (l:step ==# 'find'
                \     && (a:key ==# "\<Tab>" || a:key ==# "\<Down>"
                \         || a:key ==# "\<C-n>"))
                \ || (l:step ==# 'confirm' && a:key =~# '^[yY]$')
        if l:step ==# 'find'
            if empty(s:fr_popup.find)
                return 1
            endif
            call popup_close(a:id, {'action': 'next'})
        elseif l:step ==# 'repl'
            call popup_close(a:id, {'action': 'preview'})
        else
            call popup_close(a:id, {'action': 'apply'})
        endif
        return 1
    endif
    if l:step ==# 'confirm'
        return 1
    endif
    if s:FrEdit(a:key)
        call s:FrRender()
    endif
    return 1
endfunction

function! s:FrAdvance(timer) abort
    let l:pending = get(s:fr_popup, 'pending', {})
    if empty(l:pending)
        return
    endif
    let l:action = get(l:pending, 'action', '')
    let l:step = s:fr_popup.step
    if l:action ==# 'next'
        call s:OpenFrPopup('repl')
    elseif l:action ==# 'back'
        call s:OpenFrPopup(l:step ==# 'confirm' ? 'repl' : 'find')
    elseif l:action ==# 'preview'
        call s:ProjectReplacePreview(s:fr_popup.find, s:fr_popup.repl)
    elseif l:action ==# 'confirm'
        call s:OpenFrPopup('confirm')
    elseif l:action ==# 'apply'
        call s:ProjectReplaceApply(s:fr_popup.find, s:fr_popup.repl)
    endif
endfunction

function! s:FrClosed(id, result) abort
    let s:fr_popup.win = -1
    if type(a:result) != v:t_dict
        return
    endif
    let s:fr_popup.pending = a:result
    call timer_start(0, function('<SID>FrAdvance'))
endfunction

function! s:OpenFrPopup(step) abort
    if !has('popupwin')
        echoerr 'Replace requires +popupwin'
        return
    endif
    if s:fr_popup.win > 0 && !empty(popup_getpos(s:fr_popup.win))
        call popup_close(s:fr_popup.win, 0)
    endif
    let s:fr_popup.step = a:step
    let s:fr_popup.pending = {}
    let l:width = s:FwWidth()
    if a:step ==# 'find'
        let l:title = ' 🅕  Find '
        let l:height = 2
    elseif a:step ==# 'repl'
        let l:title = ' 🅡  Replace '
        let l:height = 3
    else
        let l:title = ' 🅒  Confirm '
        let l:height = 3
    endif
    let s:fr_popup.win = popup_create([''], {
        \ 'title': l:title,
        \ 'pos': 'center',
        \ 'minwidth': l:width,
        \ 'maxwidth': l:width,
        \ 'minheight': l:height,
        \ 'maxheight': l:height,
        \ 'border': [1, 1, 1, 1],
        \ 'borderchars': ['─', '│', '─', '│', '┌', '┐', '┘', '└'],
        \ 'padding': [0, 1, 0, 1],
        \ 'close': 'none',
        \ 'mapping': 0,
        \ 'filter': function('<SID>FrFilter'),
        \ 'callback': function('<SID>FrClosed'),
        \ 'highlight': 'MicroTerminal',
        \ 'borderhighlight': ['MicroTerminalBorder']
        \ })
    call s:FrRender()
endfunction

function! s:ProjectReplaceApply(find, repl) abort
    let l:root = substitute(fnamemodify(g:micro_workspace_root, ':p'), '/\+$', '', '')
    let l:cmd = 'rg --files-with-matches --fixed-strings --hidden --case-sensitive '
        \ . s:rg_exclude_globs . ' -- ' . shellescape(a:find) . ' '
        \ . shellescape(l:root)
    let l:files = systemlist(l:cmd)
    if v:shell_error == 2
        echoerr 'rg failed'
        return
    endif
    if empty(l:files)
        echo 'No matches to replace'
        return
    endif

    let l:pat = '\C\V' . escape(a:find, '\/')
    let l:sub = escape(a:repl, '\/~&')
    let l:buf = bufnr('%')
    let l:view = winsaveview()
    let l:written = 0
    for l:file in l:files
        if l:file !~# '^/'
            let l:file = l:root . '/' . l:file
        endif
        execute 'silent keepalt hide edit' fnameescape(l:file)
        if get(b:, 'micro_large_file', 0)
            continue
        endif
        execute printf('silent! keepjumps %%s/%s/%s/ge', l:pat, l:sub)
        if &modified
            silent update
            let l:written += 1
        endif
    endfor
    if bufexists(l:buf)
        execute 'silent keepalt buffer' l:buf
        call winrestview(l:view)
    endif
    nohlsearch
    echo printf('Replaced in %d files', l:written)
endfunction

function! s:ProjectReplacePreview(find, repl) abort
    if !s:FzfVimAvailable()
        echoerr 'fzf.vim is not available'
        return
    endif
    let l:root = substitute(fnamemodify(g:micro_workspace_root, ':p'), '/\+$', '', '')
    let l:rg = 'rg --column --line-number --no-heading --color=always '
        \ . '--fixed-strings --hidden --case-sensitive '
        \ . s:rg_exclude_globs . ' -- ' . shellescape(a:find) . ' '
        \ . shellescape(l:root)
    let l:header = 'Find: ' . a:find . '  →  ' . a:repl
        \ . '  |  ENTER continue  ESC cancel  CTRL-/ preview'
    let l:spec = fzf#vim#with_preview({
        \ 'source': l:rg,
        \ 'dir': l:root,
        \ 'options': [
        \   '--ansi',
        \   '--layout=reverse',
        \   '--info=inline-right',
        \   '--delimiter', ':',
        \   '--nth', '4..',
        \   '--prompt', 'Preview> ',
        \   '--header', l:header,
        \   '--preview-window', '+{2}/2'
        \ ]
        \ })
    let l:spec['sink*'] = function('<SID>ProjectReplacePreviewSink', [a:find, a:repl])
    call fzf#run(fzf#wrap('project-replace', l:spec, 0))
endfunction

function! s:ProjectReplacePreviewSink(find, repl, lines) abort
    if empty(a:lines)
        return
    endif
    let s:fr_popup.find = a:find
    let s:fr_popup.repl = a:repl
    let s:fr_popup.pending = {'action': 'confirm'}
    call timer_start(0, function('<SID>FrAdvance'))
endfunction

function! s:ProjectReplace(visual) abort
    if !executable('rg')
        echoerr 'Replace requires ripgrep (rg) in PATH'
        return
    endif

    let l:default = ''
    if a:visual
        let l:saved = [getreg('z'), getregtype('z')]
        silent normal! gv"zy
        let l:default = trim(substitute(getreg('z'), '\r\?\n', ' ', 'g'))
        call setreg('z', l:saved[0], l:saved[1])
    else
        let l:default = expand('<cword>')
        if l:default !~# '[[:alnum:]_]'
            let l:default = ''
        endif
    endif
    let s:fr_popup.find = l:default
    let s:fr_popup.repl = ''
    call s:OpenFrPopup('find')
endfunction

function! s:DefineRgCommands() abort
    command! -bang -nargs=* Rg
        \ call <SID>LiveRg(<q-args>, <bang>0, 0)
    command! -bang -nargs=* RgRegex
        \ call <SID>LiveRg(<q-args>, <bang>0, 1)
endfunction

" Recreate commands safely when reloading ~/.vimrc.
call s:DefineRgCommands()

" --- Git signs ---------------------------------------------------------------
" Signcolumn +/-/~ for the current buffer vs HEAD. Replaces the old popup.
" Reload-safe: stop leftover minimap popup/timer and in-flight git diff jobs.
" s:git_signs.gen drops stale job output; .cache skips unchanged files;
" .dirty queues a second refresh if the buffer changed while a job ran.
if exists('s:git_minimap_timer')
    call timer_stop(s:git_minimap_timer)
    unlet s:git_minimap_timer
endif
if exists('s:git_minimap')
    try
        if get(s:git_minimap, 'win', -1) > 0
            call popup_close(s:git_minimap.win)
        endif
    catch
    endtry
    let s:git_minimap_buf = get(s:git_minimap, 'buf', -1)
    unlet s:git_minimap
    if s:git_minimap_buf > 0 && bufexists(s:git_minimap_buf)
        execute 'silent! bwipeout!' s:git_minimap_buf
    endif
    unlet s:git_minimap_buf
endif

let s:git_signs_enabled = 1
if exists('s:git_signs')
    let s:git_signs_enabled = get(s:git_signs, 'enabled', 1)
    if has_key(s:git_signs, 'jobs')
        for s:git_sign_job in values(s:git_signs.jobs)
            try
                call job_stop(s:git_sign_job, 'kill')
            catch
            endtry
        endfor
    endif
    if get(s:git_signs, 'timer', -1) > 0
        call timer_stop(s:git_signs.timer)
    endif
endif
if has('signs')
    silent! call sign_unplace('micro_git')
    silent! call sign_unplace('beau_git')
endif
let s:git_signs = {
    \ 'enabled': s:git_signs_enabled,
    \ 'jobs': {},
    \ 'gen': {},
    \ 'out': {},
    \ 'cache': {},
    \ 'dirty': {},
    \ 'timer': -1,
    \ 'pending_buf': 0
    \ }

function! s:GitSignsSupported() abort
    return has('signs') && has('job') && executable('git')
endfunction

function! s:ApplyGitSignColors() abort
    highlight MicroGitSignAdd cterm=bold ctermfg=10 ctermbg=NONE
        \ gui=bold guifg=#9ece6a guibg=NONE
    highlight MicroGitSignChange cterm=bold ctermfg=11 ctermbg=NONE
        \ gui=bold guifg=#e0af68 guibg=NONE
    highlight MicroGitSignDelete cterm=bold ctermfg=9 ctermbg=NONE
        \ gui=bold guifg=#f7768e guibg=NONE
    if !s:GitSignsSupported()
        return
    endif
    call sign_define('MicroGitAdd', {
        \ 'text': '+ ',
        \ 'texthl': 'MicroGitSignAdd'
        \ })
    call sign_define('MicroGitChange', {
        \ 'text': '~ ',
        \ 'texthl': 'MicroGitSignChange'
        \ })
    call sign_define('MicroGitDelete', {
        \ 'text': '- ',
        \ 'texthl': 'MicroGitSignDelete'
        \ })
endfunction

function! s:GitSignsUnplace(buf) abort
    if !has('signs') || a:buf <= 0 || !bufexists(a:buf)
        return
    endif
    try
        call sign_unplace('micro_git', {'buffer': a:buf})
    catch
    endtry
    try
        call sign_unplace('beau_git', {'buffer': a:buf})
    catch
    endtry
endfunction

" Parse unified-diff hunks (git diff -U0). Cap at 2000 marks so a huge
" rewrite does not flood the signcolumn.
function! s:GitSignsParseHunks(lines, line_count) abort
    let l:count = max([1, a:line_count])
    let l:marks = {}
    for l:diff_line in a:lines
        let l:hunk = matchlist(l:diff_line,
            \ '^@@ -\(\d\+\)\%([,]\(\d\+\)\)\?'
            \ . ' +\(\d\+\)\%([,]\(\d\+\)\)\? @@')
        if empty(l:hunk)
            continue
        endif
        let l:old_count = empty(l:hunk[2]) ? 1 : str2nr(l:hunk[2])
        let l:new_start = str2nr(l:hunk[3])
        let l:new_count = empty(l:hunk[4]) ? 1 : str2nr(l:hunk[4])
        if l:new_count == 0
            let l:index = max([1, min([l:count, l:new_start])])
            let l:marks[l:index] = 'MicroGitDelete'
        else
            let l:name = l:old_count > 0 ? 'MicroGitChange' : 'MicroGitAdd'
            let l:last = min([l:count, l:new_start + l:new_count - 1])
            if l:new_start <= l:last
                for l:index in range(l:new_start, l:last)
                    let l:marks[l:index] = l:name
                endfor
            endif
        endif
        if len(l:marks) > 2000
            return {}
        endif
    endfor
    return l:marks
endfunction

function! s:GitSignsPlace(buf, marks) abort
    call s:GitSignsUnplace(a:buf)
    if empty(a:marks) || !bufexists(a:buf)
        return
    endif
    let l:list = []
    for l:lnum in keys(a:marks)
        call add(l:list, {
            \ 'buffer': a:buf,
            \ 'group': 'micro_git',
            \ 'id': str2nr(l:lnum),
            \ 'lnum': str2nr(l:lnum),
            \ 'name': a:marks[l:lnum]
            \ })
    endfor
    try
        call sign_placelist(l:list)
    catch
    endtry
endfunction

function! s:GitSignsOut(buf, gen, channel, msg) abort
    if get(s:git_signs.gen, a:buf, 0) != a:gen
        return
    endif
    if !has_key(s:git_signs.out, a:buf)
        let s:git_signs.out[a:buf] = []
    endif
    call add(s:git_signs.out[a:buf], a:msg)
endfunction

function! s:GitSignsExit(buf, gen, line_count, cache_key, job, status) abort
    if has_key(s:git_signs.jobs, a:buf)
        call remove(s:git_signs.jobs, a:buf)
    endif
    if get(s:git_signs.gen, a:buf, 0) != a:gen || !bufexists(a:buf)
        return
    endif

    let l:lines = get(s:git_signs.out, a:buf, [])
    if has_key(s:git_signs.out, a:buf)
        call remove(s:git_signs.out, a:buf)
    endif

    if empty(l:lines) || l:lines[0] ==# 'UNTRACKED'
        call s:GitSignsUnplace(a:buf)
        let s:git_signs.cache[a:buf] = a:cache_key
    else
        let l:diff = l:lines[0] ==# 'TRACKED' ? l:lines[1:] : l:lines
        let l:marks = s:GitSignsParseHunks(l:diff, a:line_count)
        call s:GitSignsPlace(a:buf, l:marks)
        let s:git_signs.cache[a:buf] = a:cache_key
    endif

    if get(s:git_signs.dirty, a:buf, 0)
        call remove(s:git_signs.dirty, a:buf)
        let s:git_signs.pending_buf = a:buf
        call s:GitSignsRefresh()
    endif
endfunction

function! s:GitSignsOnDelete(buf) abort
    let l:buf = str2nr(a:buf)
    if has_key(s:git_signs.jobs, l:buf)
        try
            call job_stop(s:git_signs.jobs[l:buf], 'kill')
        catch
        endtry
        call remove(s:git_signs.jobs, l:buf)
    endif
    for l:key in ['gen', 'out', 'cache', 'dirty']
        if has_key(s:git_signs[l:key], l:buf)
            call remove(s:git_signs[l:key], l:buf)
        endif
    endfor
endfunction

" Skip special buffers, unreadable paths, files outside a repo, and
" buffers larger than 20000 lines. Cache key is path + mtime + linecount + HEAD.
function! s:GitSignsRefresh(...) abort
    if !s:git_signs.enabled || !s:GitSignsSupported()
        return
    endif

    let l:buf = get(s:git_signs, 'pending_buf', 0)
    let s:git_signs.pending_buf = 0
    if l:buf <= 0 || !bufexists(l:buf)
        let l:buf = bufnr('%')
    endif
    if l:buf <= 0 || !bufexists(l:buf)
        return
    endif

    if getbufvar(l:buf, '&buftype') !=# ''
        return
    endif
    if getbufvar(l:buf, 'micro_large_file', 0)
            \ || getbufvar(l:buf, 'micro_long_text', 0)
        call s:GitSignsUnplace(l:buf)
        return
    endif
    let l:path = fnamemodify(bufname(l:buf), ':p')
    if empty(l:path) || !filereadable(l:path)
        call s:GitSignsUnplace(l:buf)
        return
    endif
    if s:IsDumpTreePath(l:path) || getfsize(l:path) >= 1024 * 1024
        call s:GitSignsUnplace(l:buf)
        return
    endif

    let l:root = s:GitRoot(l:path)
    if empty(l:root)
        call s:GitSignsUnplace(l:buf)
        if has_key(s:git_signs.cache, l:buf)
            call remove(s:git_signs.cache, l:buf)
        endif
        return
    endif
    let l:relative = s:GitRelativePath(l:root, l:path)
    if empty(l:relative)
        call s:GitSignsUnplace(l:buf)
        return
    endif

    let l:info = getbufinfo(l:buf)
    let l:line_count = empty(l:info) ? 0 : l:info[0].linecount
    let l:key = l:path . "\n" . string(getftime(l:path))
        \ . "\n" . string(l:line_count)
        \ . "\n" . s:GitStateStamp(l:root)
    if l:line_count > 20000
        call s:GitSignsUnplace(l:buf)
        let s:git_signs.cache[l:buf] = l:key
        return
    endif
    if get(s:git_signs.cache, l:buf, '') ==# l:key
        return
    endif
    if has_key(s:git_signs.jobs, l:buf)
        let s:git_signs.dirty[l:buf] = 1
        return
    endif

    let l:gen = get(s:git_signs.gen, l:buf, 0) + 1
    let s:git_signs.gen[l:buf] = l:gen
    let s:git_signs.out[l:buf] = []
    " argv: $1=repo root, $2=relative path. TRACKED/UNTRACKED is the first line.
    let l:job = job_start([
        \ 'sh', '-c',
        \ 'if git -C "$1" ls-files --error-unmatch -- "$2" >/dev/null 2>&1; then'
        \ . ' printf "%s\n" TRACKED;'
        \ . ' git -C "$1" diff --no-color --unified=0 HEAD -- "$2";'
        \ . ' else printf "%s\n" UNTRACKED; fi',
        \ '_', l:root, l:relative
        \ ], {
        \ 'in_io': 'null',
        \ 'err_io': 'null',
        \ 'out_mode': 'nl',
        \ 'out_cb': function('<SID>GitSignsOut', [l:buf, l:gen]),
        \ 'exit_cb': function('<SID>GitSignsExit',
        \     [l:buf, l:gen, l:line_count, l:key])
        \ })
    if job_status(l:job) ==# 'fail'
        return
    endif
    let s:git_signs.jobs[l:buf] = l:job
endfunction

function! s:GitSignsRefreshSoon(...) abort
    let s:git_signs.pending_buf = bufnr('%')
    if s:git_signs.timer > 0
        call timer_stop(s:git_signs.timer)
    endif
    let s:git_signs.timer = timer_start(80, function('<SID>GitSignsRefresh'))
endfunction

function! s:GitSignsDisable() abort
    let s:git_signs.enabled = 0
    if s:git_signs.timer > 0
        call timer_stop(s:git_signs.timer)
        let s:git_signs.timer = -1
    endif
    if has('signs')
        try
            call sign_unplace('micro_git')
        catch
        endtry
        try
            call sign_unplace('beau_git')
        catch
        endtry
    endif
    let s:git_signs.cache = {}
endfunction

function! s:GitSignsEnable() abort
    let s:git_signs.enabled = 1
    let s:git_signs.pending_buf = bufnr('%')
    call s:GitSignsRefresh()
endfunction

function! s:GitHunkJump(dir) abort
    if !has('signs')
        echo 'No git hunks'
        return
    endif
    let l:info = sign_getplaced(bufnr('%'), {'group': 'micro_git'})
    if empty(l:info) || empty(l:info[0].signs)
        echo 'No git hunks'
        return
    endif
    let l:lnums = sort(map(copy(l:info[0].signs), 'v:val.lnum'), 'n')
    let l:cur = line('.')
    if a:dir > 0
        let l:hits = filter(copy(l:lnums), 'v:val > l:cur')
        let l:dest = empty(l:hits) ? l:lnums[0] : l:hits[0]
    else
        let l:hits = filter(copy(l:lnums), 'v:val < l:cur')
        let l:dest = empty(l:hits) ? l:lnums[-1] : l:hits[-1]
    endif
    execute l:dest
    normal! zz
endfunction

call s:ApplyGitSignColors()
augroup micro_git_signs
    autocmd!
    autocmd ColorScheme * call <SID>ApplyGitSignColors()
    autocmd VimEnter,BufEnter,BufReadPost,BufWritePost,FocusGained *
        \ call <SID>GitSignsRefreshSoon()
    autocmd BufDelete,BufWipeout * call <SID>GitSignsOnDelete(expand('<abuf>'))
augroup END

" --- Plugin knobs ------------------------------------------------------------
" NERDTree file explorer; never change Vim's working directory (ChDirMode=0).
let g:NERDTreeWinSize = 35
let g:NERDTreeWinSizeMax = 40
let g:NERDTreeShowHidden = 1
let g:NERDTreeAutoCenter = 1
let g:NERDTreeChDirMode = 0
let g:NERDTreeRespectWildIgnore = 1
let g:NERDTreeMinimalUI = 1
" netrw fallback file explorer
let g:netrw_winsize   = 20
let g:netrw_liststyle = 3
let g:netrw_hide      = 1
let g:netrw_banner    = 0
let g:netrw_keepdir   = 1
let g:netrw_sort_by   = "name"

" Tagbar outline uses source order, which is more useful for C/C++.
let g:tagbar_compact = 1
let g:tagbar_autofocus = 1
let g:tagbar_sort = 0
let g:tagbar_width = 32
let g:tagbar_silent = 1

" NERDCommenter: Space-/ toggles. Default Space-c* maps are off so Space-c
" does not wait timeoutlen for cc/cu/ci.
let g:NERDSpaceDelims = 1
let g:NERDDefaultAlign = 'left'
let g:NERDCommentEmptyLines = 1
let g:NERDTrimTrailingWhitespace = 1
let g:NERDCreateDefaultMappings = 0

" AutoPairs Meta shortcuts emit garbage over SSH/tmux. Keep pair insert.
let g:AutoPairsCenterLine = 0
let g:AutoPairsShortcutToggle = ''
let g:AutoPairsShortcutFastWrap = ''
let g:AutoPairsShortcutJump = ''
let g:AutoPairsShortcutBackInsert = ''
let g:AutoPairsMoveCharacter = ''

" Copy a filesystem path to register p (and + when a clipboard exists).
" Relative form prefers the Vim-start workspace root, then cwd (fnamemodify :.).
" Space-ip pastes register p, so this works without a clipboard provider.
" Space-yr / Space-ya and NERDTree yr/ya show the copied path in a brief float.
if exists('s:notify_popup')
    if get(s:notify_popup, 'win', 0) > 0
        silent! call popup_close(s:notify_popup.win)
    endif
endif
let s:notify_popup = {'win': -1}

function! s:NotifyPopupClosed(id, result) abort
    if get(s:notify_popup, 'win', -1) == a:id
        let s:notify_popup.win = -1
    endif
endfunction

function! s:NotifyPopup(title, text) abort
    if !has('popupwin')
        echomsg a:title . ': ' . a:text
        return
    endif
    if s:notify_popup.win > 0 && !empty(popup_getpos(s:notify_popup.win))
        call popup_close(s:notify_popup.win)
    endif

    let l:width = s:FwWidth()
    let s:notify_popup.win = popup_create(a:text, {
        \ 'title': ' 🅨  ' . a:title . ' ',
        \ 'pos': 'center',
        \ 'minwidth': l:width,
        \ 'maxwidth': l:width,
        \ 'wrap': 1,
        \ 'border': [1, 1, 1, 1],
        \ 'borderchars': ['─', '│', '─', '│', '┌', '┐', '┘', '└'],
        \ 'padding': [0, 1, 0, 1],
        \ 'time': 3000,
        \ 'close': 'click',
        \ 'highlight': 'MicroTerminal',
        \ 'borderhighlight': ['MicroTerminalBorder'],
        \ 'callback': function('<SID>NotifyPopupClosed'),
        \ 'zindex': 300
        \ })
endfunction

function! s:MakeRelativePath(path) abort
    let l:abs = fnamemodify(a:path, ':p')
    let l:root = get(g:, 'micro_workspace_root', getcwd())
    if !empty(l:root)
        let l:root = substitute(fnamemodify(l:root, ':p'), '[/\\]$', '', '')
        if l:abs ==# l:root
            return '.'
        endif
        if strpart(l:abs, 0, strlen(l:root) + 1) ==# l:root . '/'
            return strpart(l:abs, strlen(l:root) + 1)
        endif
    endif
    let l:rel = fnamemodify(l:abs, ':.')
    return empty(l:rel) ? a:path : l:rel
endfunction

function! s:CopyPath(path, absolute) abort
    if empty(a:path)
        call s:NotifyPopup('Path', 'No file name')
        return
    endif

    let l:path = a:absolute ? fnamemodify(a:path, ':p') : s:MakeRelativePath(a:path)
    call setreg('p', l:path, 'v')
    if has('clipboard')
        call setreg('+', l:path, 'v')
    endif
    call s:NotifyPopup(a:absolute ? 'Absolute path' : 'Relative path', l:path)
endfunction

function! s:CopyBufferPath(absolute) abort
    if &filetype ==# 'nerdtree'
        call s:NERDTreeCopyPath(a:absolute)
        return
    endif
    call s:CopyPath(expand('%'), a:absolute)
endfunction

" Copy the selected NERDTree node path. yr = relative, ya = absolute.
function! s:NERDTreeCopyPath(absolute) abort
    try
        let l:node = g:NERDTreeFileNode.GetSelected()
        let l:path = l:node.path.str()
    catch
        call s:NotifyPopup('Path', 'NERDTree: no file or directory is selected')
        return
    endtry
    call s:CopyPath(l:path, a:absolute)
endfunction

function! s:PasteCopiedPath() abort
    if empty(getreg('p'))
        echohl WarningMsg
        echomsg 'No path has been copied yet'
        echohl None
        return
    endif
    normal! "pp
endfunction

function! s:DefineNERDTreePathMappings() abort
    nnoremap <silent><buffer> yr :call <SID>NERDTreeCopyPath(0)<CR>
    nnoremap <silent><buffer> ya :call <SID>NERDTreeCopyPath(1)<CR>
    " NERDTree binds e to OpenExpl; keep Space-e as close, not explore.
    nnoremap <silent><buffer> <Leader>e :call <SID>ToggleNERDTree()<CR>
endfunction

" Fast r / R: stock NERDTree refresh globs every cached subdirectory and
" prints "This could take a while...". Keep r recursive, but glob only
" directories whose listing actually changed. Always re-read the directory
" under the cursor (r) or the tree root (R). readdir() is the listing
" fingerprint so same-second creates are not missed the way getftime() is.
let s:nerdtree_dir_fp = {}

function! s:NERDTreeDirFingerprint(path) abort
    try
        return join(sort(readdir(a:path)), "\n")
    catch
        return ''
    endtry
endfunction

function! s:NERDTreeRefreshDir(node, force) abort
    call a:node.path.refresh(a:node.getNerdtree())
    if !a:node.path.isDirectory
        return
    endif
    " Never-opened directories have no cached children; opening them globs.
    if !a:node.isOpen && empty(a:node.children)
        return
    endif

    let l:path = a:node.path.str()
    let l:fp = s:NERDTreeDirFingerprint(l:path)
    if !a:force && get(s:nerdtree_dir_fp, l:path, "\x01") ==# l:fp
        for l:child in a:node.children
            if l:child.path.isDirectory
                call s:NERDTreeRefreshDir(l:child, 0)
            else
                call l:child.path.refresh(a:node.getNerdtree())
            endif
        endfor
        return
    endif
    let s:nerdtree_dir_fp[l:path] = l:fp

    let l:files = a:node._glob('*', 1) + a:node._glob('.*', 0)
    let l:new_children = []
    let l:invalid = []
    for l:file in l:files
        try
            let l:path_obj = g:NERDTreePath.New(l:file)
            let l:existing = a:node.getChild(l:path_obj)
            if l:existing !=# {} && l:path_obj.str() ==# l:existing.path.str()
                if l:existing.path.isDirectory
                    call s:NERDTreeRefreshDir(l:existing, 0)
                else
                    call l:existing.path.refresh(a:node.getNerdtree())
                endif
                call add(l:new_children, l:existing)
            else
                let l:new_node = g:NERDTreeFileNode.New(
                    \ l:path_obj, a:node.getNerdtree())
                let l:new_node.parent = a:node
                call add(l:new_children, l:new_node)
                call g:NERDTreePathNotifier.NotifyListeners(
                    \ 'init', l:new_node.path, l:new_node.getNerdtree(), {})
            endif
        catch /^NERDTree.\(InvalidArguments\|InvalidFiletype\)Error/
            call add(l:invalid, l:file)
        endtry
    endfor
    let a:node.children = l:new_children
    call a:node.sortChildren()
    if !empty(l:invalid)
        call nerdtree#echoWarning(
            \ len(l:invalid) . ' Invalid file(s): ' . join(l:invalid, ', '))
    endif
endfunction

function! s:NERDTreeRenderAfterRefresh() abort
    call b:NERDTree.ui.renderViewSavingPosition()
    call b:NERDTree.ui.centerView()
endfunction

function! s:NERDTreeFastRefresh(node) abort
    let l:dir = a:node
    if empty(l:dir)
        return
    endif
    if !l:dir.path.isDirectory
        let l:dir = l:dir.parent
    endif
    if empty(l:dir)
        return
    endif
    call s:NERDTreeRefreshDir(l:dir, 1)
    call s:NERDTreeRenderAfterRefresh()
endfunction

function! s:NERDTreeFastRefreshRoot() abort
    if !exists('b:NERDTree') || empty(b:NERDTree)
        return
    endif
    call s:NERDTreeRefreshDir(b:NERDTree.root, 1)
    call s:NERDTreeRenderAfterRefresh()
endfunction

function! s:NERDTreeInstallFastRefresh() abort
    if !exists('*NERDTreeAddKeyMap')
        return
    endif
    call NERDTreeAddKeyMap({
        \ 'key': g:NERDTreeMapRefresh,
        \ 'scope': 'Node',
        \ 'callback': function('<SID>NERDTreeFastRefresh'),
        \ 'quickhelpText': 'refresh cursor dir',
        \ 'override': 1
        \ })
    call NERDTreeAddKeyMap({
        \ 'key': g:NERDTreeMapRefreshRoot,
        \ 'scope': 'all',
        \ 'callback': function('<SID>NERDTreeFastRefreshRoot'),
        \ 'quickhelpText': 'refresh current root',
        \ 'override': 1
        \ })
endfunction

if exists('*NERDTreeAddKeyMap')
    call s:NERDTreeInstallFastRefresh()
endif

" Space-e: close an open tree (from the file window or the tree itself).
" Opening with a named file buffer reveals that node instead of landing on
" the tree root. Unnamed / special buffers still use a plain toggle.
" A fullscreen tree (vim .) is the last window: NERDTreeClose would E444,
" so replace it with an empty buffer instead.
function! s:NERDTreeWinCount() abort
    let l:n = 0
    for l:win in range(1, winnr('$'))
        if getwinvar(l:win, '&filetype') ==# 'nerdtree'
            let l:n += 1
        endif
    endfor
    return l:n
endfunction

function! s:ToggleNERDTree() abort
    let l:trees = s:NERDTreeWinCount()
    if l:trees > 0
        if winnr('$') <= l:trees
            enew
            return
        endif
        NERDTreeClose
        return
    endif
    " expand('%:p') on an unnamed buffer can be the cwd; require a name.
    if &buftype ==# '' && !empty(expand('%'))
        NERDTreeFind
        return
    endif
    NERDTreeToggle
endfunction

" Color a small set of extensions in NERDTree. Tokyo Night fg colors.
" .c/.cc/.cpp share blue; .S/.s share gold. Patterns are case-sensitive.
let s:nerdtree_ext_hl = [
    \ ['rs',  'MicroNerdTreeRs'],
    \ ['c',   'MicroNerdTreeC'],
    \ ['cc',  'MicroNerdTreeCc'],
    \ ['cpp', 'MicroNerdTreeCpp'],
    \ ['h',   'MicroNerdTreeH'],
    \ ['S',   'MicroNerdTreeAsm'],
    \ ['s',   'MicroNerdTreeAsm'],
    \ ['py',  'MicroNerdTreePy'],
    \ ['sh',  'MicroNerdTreeSh'],
    \ ['gn',  'MicroNerdTreeGn'],
    \ ['md',  'MicroNerdTreeMd'],
    \ ['txt', 'MicroNerdTreeTxt'],
    \ ['rst', 'MicroNerdTreeRst']
    \ ]

function! s:ApplyNERDTreeFileColors() abort
    highlight MicroNerdTreeRs  ctermfg=209 guifg=#ff9e64
    highlight MicroNerdTreeC   ctermfg=12  guifg=#7aa2f7
    highlight MicroNerdTreeCc  ctermfg=12  guifg=#7aa2f7
    highlight MicroNerdTreeCpp ctermfg=12  guifg=#7aa2f7
    highlight MicroNerdTreeH   ctermfg=14  guifg=#7dcfff
    highlight MicroNerdTreeAsm ctermfg=11  guifg=#e0af68
    highlight MicroNerdTreePy  ctermfg=11  guifg=#e0af68
    highlight MicroNerdTreeSh  ctermfg=10  guifg=#9ece6a
    highlight MicroNerdTreeGn  ctermfg=13  guifg=#bb9af7
    highlight MicroNerdTreeMd  ctermfg=15  guifg=#c0caf5
    highlight MicroNerdTreeTxt ctermfg=8   guifg=#a9b1d6
    highlight MicroNerdTreeRst ctermfg=15  guifg=#c0caf5
endfunction

function! s:NERDTreeDefineFileSyntax() abort
    if &filetype !=# 'nerdtree'
        return
    endif
    for [l:ext, l:group] in s:nerdtree_ext_hl
        execute 'silent! syntax clear' l:group
    endfor
    for [l:ext, l:group] in s:nerdtree_ext_hl
        execute 'syntax match' l:group '/\v\S+\.' . l:ext . '$/ containedin=NERDTreeFile'
        execute 'syntax match' l:group '/\v\S+\.' . l:ext . '\W*\*$/ containedin=NERDTreeExecFile'
    endfor
    call s:ApplyNERDTreeFileColors()
endfunction

function! s:NERDTreeDefineFileSyntaxOpen() abort
    for l:info in getbufinfo()
        if getbufvar(l:info.bufnr, '&filetype') !=# 'nerdtree'
            continue
        endif
        for l:win in l:info.windows
            call win_execute(l:win, 'call <SID>NERDTreeDefineFileSyntax()')
            break
        endfor
    endfor
endfunction

augroup micro_nerdtree_paths
    autocmd!
    autocmd FileType nerdtree call <SID>DefineNERDTreePathMappings()
    autocmd FileType nerdtree call <SID>NERDTreeInstallFastRefresh()
    autocmd FileType nerdtree call <SID>NERDTreeDefineFileSyntax()
    autocmd VimEnter * call <SID>NERDTreeInstallFastRefresh()
    autocmd ColorScheme * call <SID>ApplyNERDTreeFileColors()
augroup END
call s:ApplyNERDTreeFileColors()
call s:NERDTreeDefineFileSyntaxOpen()

" --- Commands and leader maps ------------------------------------------------
function! s:LeaderFHint() abort
    echo 'fe files  fg grep  fw word[+dir]  fr replace  fl lines  fb buffers  fh recent  fc commands'
endfunction

function! s:WorkspaceFiles() abort
    if !s:FzfVimAvailable()
        echoerr 'fzf.vim is not available'
        return
    endif
    execute 'Files' fnameescape(g:micro_workspace_root)
endfunction

function! s:BufferLines() abort
    " Each token is an exact substring.  'sos kick nebula' keeps lines that
    " contain all three strings, and drops fuzzy hits such as 'sxxxx oxs'.
    if !s:FzfVimAvailable()
        echoerr 'fzf.vim is not available'
        return
    endif
    call fzf#vim#buffer_lines({
        \ 'options': ['--exact', '--prompt', 'Lines> ']
        \ })
endfunction

function! s:FormatBuffer() abort
    if empty(&formatprg)
        echo 'No formatprg for this filetype'
        return
    endif
    let l:view = winsaveview()
    silent execute 'keepjumps normal! gggqG'
    call winrestview(l:view)
    echo 'Formatted with ' . &formatprg
endfunction

" Same directory first, then walk parents with findfile(). Not a project-wide
" ctags lookup; use Space-g / Space-s for that. Mapped to Space-x (h is help).
function! s:SwitchHeaderSource() abort
    let l:name = expand('%:t')
    if empty(l:name)
        echo 'No file name'
        return
    endif

    let l:stem = expand('%:t:r')
    let l:ext = expand('%:e')
    if l:ext =~? '^\(c\|cc\|cpp\|cxx\|c++\|C\)$'
        let l:alts = [l:stem . '.h', l:stem . '.hpp', l:stem . '.hh', l:stem . '.hxx']
    elseif l:ext =~? '^\(h\|hpp\|hh\|hxx\|H\)$'
        let l:alts = [l:stem . '.c', l:stem . '.cc', l:stem . '.cpp', l:stem . '.cxx']
    else
        echo 'Not a C/C++ header or source file'
        return
    endif

    let l:dir = expand('%:p:h')
    for l:alt in l:alts
        let l:same = l:dir . '/' . l:alt
        if filereadable(l:same)
            execute 'edit' fnameescape(l:same)
            return
        endif
    endfor
    for l:alt in l:alts
        let l:found = findfile(l:alt, l:dir . ';')
        if !empty(l:found)
            execute 'edit' fnameescape(fnamemodify(l:found, ':p'))
            return
        endif
    endfor
    echo 'No matching header/source for ' . l:name
endfunction

" Prefer GNU GLOBAL when GTAGS exists in a parent of the current file.
function! s:JumpGtagsCursor() abort
    if !empty(findfile('GTAGS', '.;')) && exists(':GtagsCursor')
        GtagsCursor
        return
    endif
    call s:FzfTagUnderCursor()
endfunction

function! s:GtagsRefs() abort
    let l:word = expand('<cword>')
    if empty(l:word)
        echo 'No symbol under cursor'
        return
    endif
    if !exists(':Gtags')
        echo 'gtags plugin is not available'
        return
    endif
    execute 'Gtags -r' l:word
endfunction

function! s:ToggleQuickfix() abort
    for l:win in getwininfo()
        if l:win.quickfix && !l:win.loclist
            cclose
            return
        endif
    endfor
    botright copen 10
endfunction

function! s:ListJump(cmd) abort
    try
        execute a:cmd
        normal! zz
    catch /^Vim\%((\a\+)\)\=:E\%(42\|553\)/
        echo 'No more items'
    endtry
endfunction

" Run make from the Vim-start workspace root so :cd / NERDTree do not
" change the build directory. Arguments are make targets (e.g. :Make verify).
function! s:WorkspaceMake(...) abort
    let l:root = get(g:, 'micro_workspace_root', getcwd())
    if !isdirectory(l:root)
        echoerr 'No workspace root'
        return
    endif
    let l:save = getcwd()
    execute 'lcd' fnameescape(l:root)
    try
        execute 'silent make!' join(map(copy(a:000), 'fnameescape(v:val)'))
    finally
        execute 'lcd' fnameescape(l:save)
    endtry
    botright cwindow
    redraw!
endfunction

function! s:FzfTagUnderCursor() abort
    let l:symbol = expand('<cword>')
    if empty(l:symbol)
        echo 'No symbol under cursor'
        return
    endif
    if !s:FzfVimAvailable()
        echoerr 'fzf.vim is not available'
        return
    endif
    call fzf#vim#tags(l:symbol, fzf#vim#with_preview({
        \ 'placeholder': '--tag {2}:{-1}:{3..}'
        \ }), 0)
endfunction

command! Format call <SID>FormatBuffer()
command! CopyRelPath call <SID>CopyBufferPath(0)
command! CopyAbsPath call <SID>CopyBufferPath(1)
command! ProjectReplace call <SID>ProjectReplace(0)
command! -nargs=* Make call <SID>WorkspaceMake(<f-args>)
command! -nargs=0 Debug packadd termdebug | Termdebug

" Space-h: scrollable shortcut manual. Same width as Space-fr.
if exists('s:help_popup')
    if get(s:help_popup, 'win', 0) > 0
        silent! call popup_close(s:help_popup.win)
    endif
endif
let s:help_popup = {'win': -1, 'nlines': 0}

function! s:HelpManualLines() abort
    return [
        \ 'Space is <Leader>.  j/k  C-d/C-u  g/G  q/Esc/h close',
        \ '',
        \ 'Find',
        \ '  Space-fe          workspace files',
        \ '  Space-fg          live ripgrep, literal  (visual: selection)',
        \ '  Space-fw          word under cursor; optional subdir float',
        \ '  Space-re          live ripgrep, regex',
        \ '  Space-fl          buffer lines, exact substrings',
        \ '  Space-fb          buffers',
        \ '  Space-fh          recent files',
        \ '  Space-fc          commands',
        \ '  Space-f           short find hint',
        \ '',
        \ 'Replace',
        \ '  Space-fr          find float → replace float → preview',
        \ '                    → Confirm (ENTER/y apply, Esc/n back)',
        \ '',
        \ 'Files and windows',
        \ '  Space-e           NERDTree toggle / find current file',
        \ '  Space-t           floating terminal',
        \ '  Space-w           next window',
        \ '  Ctrl-w h/l        window left / right',
        \ '  Space-o           Tagbar',
        \ '  Space-q           quickfix toggle',
        \ '  Space-x           C/C++ header ↔ source',
        \ '  Space-h           this manual',
        \ '',
        \ 'Code',
        \ '  Space-g           fzf tags for word under cursor',
        \ '  Space-s           GtagsCursor, else fzf tags',
        \ '  Space-a           Gtags -r (references)',
        \ '  Space-=           format buffer (formatprg)',
        \ '  Space-/           toggle comment',
        \ '  Space-b           jump back',
        \ '  Space-d / u       next / previous paragraph',
        \ '  Space-n / m       presentation view / restore numbers+signs',
        \ '',
        \ 'Editing',
        \ '  Y                 yank to end of line',
        \ '  * / #             search word, keep cursor centered',
        \ '  visual * / #      search selection',
        \ '  visual p          paste without clobbering clipboard',
        \ '  visual < / >      indent and keep selection',
        \ '  Ctrl-j / Ctrl-k   window down / up',
        \ '  ]q / [q           next / previous quickfix  (]Q [Q first/last)',
        \ '  ]h / [h           next / previous git hunk',
        \ '',
        \ 'Paths',
        \ '  Space-yr          copy relative path (float)',
        \ '  Space-ya          copy absolute path (float)',
        \ '  Space-ip          paste copied path (register p)',
        \ '',
        \ 'Other keys',
        \ '  Ctrl-L            clear search highlight, refresh diff',
        \ '  n / N             search next / prev, keep cursor centered',
        \ '  Ctrl-N / Ctrl-P   insert completion',
        \ '',
        \ 'Floating terminal',
        \ '  Space-t           show / hide',
        \ '  Ctrl-Q            hide, keep the shell',
        \ '  Esc Esc           close and wipe the shell',
        \ '',
        \ 'NERDTree',
        \ '  yr / ya           copy node relative / absolute path',
        \ '  r / R             refresh cursor dir / tree root',
        \ '  Space-e           close tree',
        \ '  colors            rs cc/cpp/c h S/s py sh gn md txt rst',
        \ '',
        \ 'Commands',
        \ '  :MICROStyle       tabs 8 (auto under zircon/)',
        \ '  :GOOGLEStyle      spaces 2 (auto under userspace/)',
        \ '  :LINUXStyle       tabs 8    :Format       formatprg',
        \ '  :Make [target]    make from workspace root → quickfix',
        \ '  :Rg  :RgRegex     :ProjectReplace  :FloatTerminal  :Debug',
        \ ]
endfunction

function! s:HelpPopupClosed(id, result) abort
    if get(s:help_popup, 'win', -1) == a:id
        let s:help_popup.win = -1
    endif
endfunction

function! s:HelpPopupVisible() abort
    if s:help_popup.win <= 0
        return 0
    endif
    let l:pos = popup_getpos(s:help_popup.win)
    return !empty(l:pos) && get(l:pos, 'visible', 0)
endfunction

function! s:HelpPopupSize() abort
    let l:width = s:FwWidth()
    let l:maxh = max([8, min([float2nr((&lines - 2) * 0.70), &lines - 4])])
    let l:height = max([8, min([s:help_popup.nlines, l:maxh])])
    return {
        \ 'minwidth': l:width,
        \ 'maxwidth': l:width,
        \ 'minheight': l:height,
        \ 'maxheight': l:height
        \ }
endfunction

function! s:HelpScroll(id, delta) abort
    let l:pos = popup_getpos(a:id)
    if empty(l:pos)
        return
    endif
    let l:maxfirst = max([1, s:help_popup.nlines - get(l:pos, 'core_height', 1) + 1])
    let l:first = max([1, min([l:pos.firstline + a:delta, l:maxfirst])])
    call popup_setoptions(a:id, {'firstline': l:first})
endfunction

function! s:HelpFilter(id, key) abort
    if a:key ==# "\<Esc>" || a:key ==# 'q' || a:key ==# "\<C-c>"
                \ || a:key ==# 'h' || a:key ==# "\<CR>"
        call popup_close(a:id, 0)
        return 1
    endif
    if a:key ==# 'j' || a:key ==# "\<Down>" || a:key ==# "\<C-n>"
                \ || a:key ==# "\<C-e>"
        call s:HelpScroll(a:id, 1)
        return 1
    endif
    if a:key ==# 'k' || a:key ==# "\<Up>" || a:key ==# "\<C-p>"
                \ || a:key ==# "\<C-y>"
        call s:HelpScroll(a:id, -1)
        return 1
    endif
    if a:key ==# "\<ScrollWheelDown>"
        call s:HelpScroll(a:id, 3)
        return 1
    endif
    if a:key ==# "\<ScrollWheelUp>"
        call s:HelpScroll(a:id, -3)
        return 1
    endif
    if a:key ==# "\<C-d>" || a:key ==# "\<PageDown>" || a:key ==# "\<C-f>"
        let l:pos = popup_getpos(a:id)
        call s:HelpScroll(a:id, max([1, get(l:pos, 'core_height', 8) / 2]))
        return 1
    endif
    if a:key ==# "\<C-u>" || a:key ==# "\<PageUp>" || a:key ==# "\<C-b>"
        let l:pos = popup_getpos(a:id)
        call s:HelpScroll(a:id, -max([1, get(l:pos, 'core_height', 8) / 2]))
        return 1
    endif
    if a:key ==# 'g'
        call popup_setoptions(a:id, {'firstline': 1})
        return 1
    endif
    if a:key ==# 'G'
        call s:HelpScroll(a:id, s:help_popup.nlines)
        return 1
    endif
    return 1
endfunction

function! s:ResizeHelpPopup() abort
    if s:HelpPopupVisible()
        call popup_setoptions(s:help_popup.win, s:HelpPopupSize())
    endif
endfunction

function! s:ToggleHelp() abort
    if !has('popupwin')
        echoerr 'Help requires +popupwin'
        return
    endif
    if s:HelpPopupVisible()
        call popup_close(s:help_popup.win, 0)
        let s:help_popup.win = -1
        return
    endif

    let l:lines = s:HelpManualLines()
    let s:help_popup.nlines = len(l:lines)
    let l:size = s:HelpPopupSize()
    let s:help_popup.win = popup_create(l:lines, extend(l:size, {
        \ 'title': ' 🅗  Help ',
        \ 'pos': 'center',
        \ 'border': [1, 1, 1, 1],
        \ 'borderchars': ['─', '│', '─', '│', '┌', '┐', '┘', '└'],
        \ 'padding': [0, 1, 0, 1],
        \ 'close': 'none',
        \ 'mapping': 0,
        \ 'scrollbar': 1,
        \ 'filter': function('<SID>HelpFilter'),
        \ 'callback': function('<SID>HelpPopupClosed'),
        \ 'highlight': 'MicroTerminal',
        \ 'borderhighlight': ['MicroTerminalBorder'],
        \ 'zindex': 300
        \ }))
endfunction

command! MicroHelp call <SID>ToggleHelp()

augroup micro_help_popup
    autocmd!
    autocmd VimResized * call <SID>ResizeHelpPopup()
augroup END

" Leader map index (Space = <Leader>)
"   h  shortcut manual          e  NERDTree toggle/find     f  find hint
"   fe workspace files          fg live rg (text)     fw word (+ optional subdir)
"   fr replace (find, replace, confirm)  fl buffer lines (exact)  fb buffers
"   fh recent files             fc commands           re live rg (regex)
"   t  floating terminal        w  next window (Ctrl-w h/l for left/right)
"   g  fzf tags                 s  GtagsCursor / tags a  Gtags -r
"   o  Tagbar                   x  header <-> source  =  formatprg
"   q  quickfix                 /  comment toggle     b  jump back
"   d  next paragraph           u  prev paragraph
"   n  presentation view        m  restore editing view
"   yr relative path            ya absolute path      ip paste path (register p)
" ]q [q quickfix  ]h [h git hunks  Ctrl-j/k windows  Y yank-to-eol
" :Make [target]  from workspace root.  :MICROStyle / :GOOGLEStyle / :LINUXStyle
" NERDTree buffer: yr/ya copy node path, r refresh cursor dir, R refresh root.
function! s:VisualStar() abort
    let l:saved = [getreg('"'), getregtype('"')]
    normal! gvy
    let l:pat = '\V' . substitute(escape(@", '\'), '\n', '\\n', 'g')
    call setreg('"', l:saved[0], l:saved[1])
    let @/ = l:pat
    call histadd('/', l:pat)
    let v:hlsearch = 1
endfunction

nnoremap <silent> <C-l> :nohlsearch<Bar>diffupdate<CR><C-l>
nnoremap <C-j> <C-w>j
nnoremap <C-k> <C-w>k
nnoremap n nzzzv
nnoremap N Nzzzv
nnoremap * *zzzv
nnoremap # #zzzv
nnoremap g* g*zzzv
nnoremap g# g#zzzv
nnoremap Y y$
xnoremap < <gv
xnoremap > >gv
xnoremap p P
xnoremap <silent> * :<C-u>call <SID>VisualStar()<CR>nzzzv
xnoremap <silent> # :<C-u>call <SID>VisualStar()<CR>Nzzzv
inoremap <C-u> <C-g>u<C-u>
inoremap <C-w> <C-g>u<C-w>
nnoremap <silent> ]q :call <SID>ListJump('cnext')<CR>
nnoremap <silent> [q :call <SID>ListJump('cprevious')<CR>
nnoremap <silent> ]Q :call <SID>ListJump('clast')<CR>
nnoremap <silent> [Q :call <SID>ListJump('cfirst')<CR>
nnoremap <silent> ]l :call <SID>ListJump('lnext')<CR>
nnoremap <silent> [l :call <SID>ListJump('lprevious')<CR>
nnoremap <silent> ]h :call <SID>GitHunkJump(1)<CR>
nnoremap <silent> [h :call <SID>GitHunkJump(-1)<CR>
nnoremap <silent> <Leader>ip :call <SID>PasteCopiedPath()<CR>
nnoremap <silent> <Leader>yr :call <SID>CopyBufferPath(0)<CR>
nnoremap <silent> <Leader>ya :call <SID>CopyBufferPath(1)<CR>
nnoremap <silent> <Leader>e :call <SID>ToggleNERDTree()<CR>
nnoremap <silent> <Leader>f :call <SID>LeaderFHint()<CR>
xnoremap <silent> <Leader>f :<C-u>call <SID>LeaderFHint()<CR>
nnoremap <silent> <Leader>fe :call <SID>WorkspaceFiles()<CR>
nnoremap <silent> <Leader>fg :Rg<CR>
nnoremap <silent> <Leader>fw :call <SID>LiveRgWord()<CR>
nnoremap <silent> <Leader>fr :call <SID>ProjectReplace(0)<CR>
xnoremap <silent> <Leader>fr :<C-u>call <SID>ProjectReplace(1)<CR>
xnoremap <silent> <Leader>fg :<C-u>call <SID>LiveRgVisual()<CR>
nnoremap <silent> <Leader>fl :call <SID>BufferLines()<CR>
nnoremap <silent> <Leader>fb :Buffers<CR>
nnoremap <silent> <Leader>fh :History<CR>
nnoremap <silent> <Leader>fc :Commands<CR>
nnoremap <silent> <Leader>re :RgRegex<CR>
nnoremap <silent> <Leader>t <Cmd>FloatTerminal<CR>
nnoremap <silent> <Leader>w <C-w><C-w>
nnoremap <silent> <Leader>g <Cmd>call <SID>FzfTagUnderCursor()<CR>
nnoremap <silent> <Leader>s :call <SID>JumpGtagsCursor()<CR>
nnoremap <silent> <Leader>a :call <SID>GtagsRefs()<CR>
nnoremap <silent> <Leader>o :TagbarToggle<CR>
nnoremap <silent> <Leader>h :call <SID>ToggleHelp()<CR>
xnoremap <silent> <Leader>h :<C-u>call <SID>ToggleHelp()<CR>
nnoremap <silent> <Leader>x :call <SID>SwitchHeaderSource()<CR>
nnoremap <silent> <Leader>= :Format<CR>
nnoremap <silent> <Leader>q :call <SID>ToggleQuickfix()<CR>
nmap <silent> <Leader>/ <Plug>NERDCommenterToggle
xmap <silent> <Leader>/ <Plug>NERDCommenterToggle
nnoremap <silent> <Leader>b <C-o>
nnoremap <silent> <Leader>d }
nnoremap <silent> <Leader>u {
nnoremap <silent> <Leader>n
    \ :set nonumber norelativenumber nolist mouse= signcolumn=no<CR>
    \ :call <SID>GitSignsDisable()<CR>
nnoremap <silent> <Leader>m
    \ :set number relativenumber list mouse=a signcolumn=yes<CR>
    \ :call <SID>GitSignsEnable()<CR>
" Insert completion uses Vim's built-in Ctrl-N and Ctrl-P.

" Re-apply buffer-local session state after :source (clipboard dump
" session, git signs that were unplaced, overlay highlights).
if exists('*BeauStatusMode')
    delfunction BeauStatusMode
endif
if exists('*BeauGitBranch')
    delfunction BeauGitBranch
endif
call s:LargeFileWin()
call s:GitSignsRefreshSoon()
