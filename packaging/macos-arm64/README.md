# vis 快速安裝包：macOS Apple Silicon

適用於 Apple Silicon（M1／M2／M3／M4 等）與 macOS **13.5 以上**。
編輯器、Lua、LPeg、Node 和常用語言伺服器均隨包提供，安裝過程不需要
網路、Homebrew、sudo 或自行編譯。請使用原生 arm64 終端機，避免 Rosetta。

## 快速安裝

將 `vis-…-macos-arm64.tar.gz` 與同名 `.sha256` 檔案放在一起，在終端機執行：

```sh
shasum -a 256 -c vis-*-macos-arm64.tar.gz.sha256
tar -xzf vis-*-macos-arm64.tar.gz
cd vis-*-macos-arm64
./install.sh
source "${ZDOTDIR:-$HOME}/.zshrc"
vise your-file.py
```

以上萬用字元假設所在目錄只有一個版本；若有多個版本，請使用完整檔名。
`vis`、`vise` 都會啟動這套編輯器。系統工具仍可用 `/usr/bin/vis` 呼叫。
安裝器會檢查套件內所有一般檔案的 SHA-256；壓縮包的 `.sha256` 用來核對下載是否完整，
不是開發者身分或公證證明。此版本未經 Apple notarization；若系統封鎖下載的程式，
請先確認來源，再透過「系統設定 → 隱私權與安全性」允許開啟。

可以直接試用解壓後的編輯器：先執行安裝以放入預設設定；單獨執行
`payload/launch.sh` 則會使用目前帳號已有的 vis 設定。

## 包含內容

- 原生 arm64 vis，靜態內建 Lua 5.5.1、LPeg 1.1.0。
- Zenburn 深色主題、自動語法高亮、行號、游標行、自動縮排、4 格 tab。
- vis-lspc 與支援 Unicode 的 dkjson。
- Node 24.22.1、Lua language server 3.19.2、Pyright。
- TypeScript language server 與獨立 TypeScript 5.9.3，不改動全域 Node/npm。
- HTML、CSS、JSON、YAML 語言伺服器。
- `vis-menu` 等原有輔助工具；已有 fzf 時外掛會優先使用它。

C/C++ 使用 Apple Command Line Tools 的 clangd。若尚未安裝：

```sh
xcode-select --install
```

這個 Apple 安裝步驟需要網路。實際使用 C/C++ LSP 也需要有效的 Xcode／CLT 開發工具路徑；
專案可提供 `compile_commands.json` 或 `.clangd` 指定編譯參數。
Go、Rust 已提供設定，若 PATH 上有 `gopls`、`rust-analyzer` 就會自動啟用；
這兩種語言的工具鏈不隨包提供。

## 設定、升級與備份

預設位置：

| 項目 | 位置 |
| --- | --- |
| 啟動命令 | `~/.local/bin/vis`、`~/.local/bin/vise` |
| 可自行修改的設定 | `${XDG_CONFIG_HOME:-~/.config}/vis/visrc.lua` |
| 安裝版本與 current 連結 | `~/.local/share/vis-quick/` |
| 舊檔案備份 | `~/.local/share/vis-quick/backups/日期.隨機字串/` |
| shell aliases | `${ZDOTDIR:-$HOME}/.zshrc` 的 vis-quick 區塊 |

重新執行安裝器即可升級／重裝。每次都會先備份將被取代的 `vis`、`vise`、
`visrc.lua` 與 shell 設定；安裝器會重新放入預設 visrc，舊的自訂內容可從備份取回。
其餘 vis plugins、其他編輯器 alias 與設定檔不會刪除。
不想修改 shell，或需要其他安裝路徑：

```sh
./install.sh --no-shell
./install.sh --prefix "$HOME/Applications/vis-local"
```

自訂 prefix 必須是絕對路徑；支援路徑含空白，不支援冒號或換行。
使用 `--no-shell` 後，可用 `~/.local/bin/vise` 的完整路徑啟動。

還原：從該次安裝輸出的備份資料夾取回需要的檔案至原位置。shell 備份名稱為
`zshrc`，設定檔備份名稱為 `visrc.lua`；若原先沒有檔案，就不會有對應備份。
`previous-current`（若存在）記錄上一次套件版本位置。移除時，先還原舊啟動檔與設定，
移除 shell 的 vis-quick 區塊，再刪除 `share/vis-quick`；請保留仍要使用的備份。

## 常用操作

| 按鍵／指令 | 功能 |
| --- | --- |
| `gd` / `gD` | 定義／宣告 |
| `gr` | 查詢引用 |
| `K` | hover 文件 |
| `Ctrl-t` | 返回跳轉前位置 |
| `Ctrl-l` | 補全；一般與插入模式皆可 |
| `Space e` | 本行診斷 |
| `Space n` / `Space N` | 下一個／上一個診斷 |
| `:lspc-rename 新名稱` | 重新命名符號 |
| `:lspc-format` | 格式化，依伺服器能力而定 |
| `:w` | 儲存並同步內容、更新診斷 |

LSP 會在開啟相應檔案時啟動。vis-lspc 在儲存或發送 LSP 請求前同步完整內容，
診斷不是每次按鍵都即時更新。

## 從原始碼產生安裝包

建置端需要 Apple Silicon Mac、Xcode Command Line Tools、Python 3.12+、Git、npm。
建置會下載固定版本並核對 SHA-256，npm 則使用已提交的 lockfile。
編譯來源是目前 Git HEAD；打包腳本與設定取自工作目錄。建置不會安裝到建置者帳號，
也不會改寫來源樹的 `config.mk`、`config.h` 或既有 binary。

```sh
python3 packaging/macos-arm64/build.py
python3 packaging/macos-arm64/smoke.py dist/vis-*-macos-arm64.tar.gz
```

產物放在 `dist/`，下載快取位於 `.package-cache/`；兩者都已加入 `.gitignore`。
封裝內附 `MANIFEST.json`、npm lockfile、licenses 與 vis／Lua／LPeg／vis-lspc 原始碼。
建置會檢查 Mach-O 架構及動態連結，拒絕依賴 Homebrew 或使用者目錄的核心 binary。
最低系統版本由編譯 target 與官方 binary 的 load commands 核對；實際執行驗證
仍以建置／測試機的 macOS 版本為準，不代表已逐一測過所有舊版系統。

上游：[vis](https://github.com/martanne/vis)、
[vis-lspc](https://codeberg.org/muhq/vis-lspc)、[Lua](https://www.lua.org/)、
[LPeg](https://www.inf.puc-rio.br/~roberto/lpeg/)、[Node.js](https://nodejs.org/)、
[LuaLS](https://github.com/LuaLS/lua-language-server)、[dkjson](https://dkolf.de/dkjson-lua/)。
