# Chat

A desktop chat client for [Nyx AI Core](https://github.com/design-nexus/nyx-ai) (the suite's shared llama.cpp runtime) and optional [Ollama](https://ollama.com), built with GTK 4. It chats with
local models, lets them use tools such as web search, and downloads GGUF models from Hugging Face. It takes its colors
from your [Omarchy](https://omarchy.org) theme and fits a half-screen tile.

Chat uses Nyx AI Core on this computer by default. It loads, schedules and downloads models for every Nyx app; Chat keeps its own chats, prompts and knowledge bases.

## What it does

- **Chat**
  - The model picker sits above the conversation, with a bar under it that pulses
    while the model loads and fills once it's in memory. The model you pick stays
    loaded until you pick another, and new chats start with it (both can be changed in
    Settings).
  - Replies stream in with Markdown, tables, syntax-highlighted code (with a copy
    button), and formulas shown as readable symbols.
  - Thinking models show their reasoning in a collapsible block.
  - Stop a reply with <kbd>Esc</kbd>, regenerate it, or edit a question and send it
    again. Every version is kept; arrows under a message switch between them.
  - **Continue** a reply that was stopped or hit the length limit, **edit** a reply so
    later turns build on your version, or **branch** a new chat from any message.
  - Select text in a reply to **ask about it** or have it explained.
  - Click a reply's speed to see its time to first token, token counts, total time and
    why it ended. The header shows the model and its speed, and the message bar
    shows how much of the context the chat uses.
  - When a chat outgrows its context, the oldest messages are left out (or the first
    exchange is kept, or the chat stops and says so), per chat or by default.
  - Optional follow-up questions after each reply.
  - **Temporary chats** (<kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>N</kbd>) aren't saved or
    searchable and are deleted when you leave them.
  - **Compare:** answer the same message with up to three models side by side, then
    continue with the answer you like.
  - **Attachments:** images for vision models (drop, paste or attach), and text, code or
    PDF files. Long files are searched for the relevant parts when an embedding model is
    installed.
  - **Screenshot:** <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>S</kbd> lets you drag out an
    area of the screen and attaches it.
  - Per-chat system prompt, temperature, top P, top K, context length, seed and thinking,
    in a side panel.
  - **Structured output:** ask for JSON, optionally matching a JSON schema. JSON replies
    are shown as a tree.
  - **Previews:** SVG code blocks are drawn in the side panel; HTML and Mermaid blocks
    open in your browser.
  - Type <kbd>/</kbd> for saved prompts, <kbd>@</kbd> to switch model, <kbd>#</kbd> to add
    a knowledge collection.
  - Chats get a title automatically, and can be renamed, pinned, tagged, archived,
    exported as Markdown or JSON, and searched by their full text. Pinned chats and
    folders have their own groups in the sidebar.
  - **Folders:** each folder can set the model, system prompt, tools and knowledge its
    chats start with.
- **Tools** for models that support them:
  - Web search (DuckDuckGo with no setup, or SearXNG, Brave Search or Tavily), with
    numbered citations that link to their sources.
  - Reading web pages, the date and time, and a calculator.
  - Location (off by default): from the desktop location service (GeoClue), roughly from
    your IP address, or a place you type.
  - Reading files, only inside folders you allow.
  - Running shell commands, with your approval each time.
  - Saving memories, with your approval.
  - Tools from [MCP](https://modelcontextprotocol.io) servers that run on this computer.
  - Models without tool support still get web results: the search runs first and the
    results go into the prompt.
- **Memories:** facts about you that every chat knows.
- **Knowledge:** collections of documents embedded with a model on the selected backend. Existing collections are re-embedded from their saved text when first used with a new backend.
- **Models:** installed, loaded, and downloading models on the selected server, with load, unload, delete and details. Ollama also offers copy, create, and registry update checks.
- **Marketplace:**
  - Search Hugging Face GGUF repositories by trending, downloads, likes or date, and
    filter to text, vision or embedding models.
  - When using Ollama, search its library and see every tag with its size.
  - Each download is marked **Fits GPU**, **Partial offload** or **Too big** for this
    computer, counting the memory for the context length set in Settings, and the best
    fit is marked **Recommended**.
  - Model cards are shown in full. Downloads queue up with progress, speed and time left.
- **Arena:** two anonymous models answer the same prompt; you vote, and an Elo
  leaderboard keeps score.
- **Stats:** tokens and chats per day, and replies and speed per model.
- **Voice:** dictate with [whisper.cpp](https://github.com/ggml-org/whisper.cpp)
  (<kbd>Ctrl</kbd>+<kbd>Space</kbd>) and hear replies read aloud with
  [Piper](https://github.com/rhasspy/piper). Hands-free mode sends when you stop talking.
- **Quick ask:** `nyx-chat --quick` opens a small window for one-off questions, optionally
  about the text you've highlighted or copied. **Open in chat** keeps the exchange.
- **Several servers:** add llama.cpp or Ollama servers on other computers and switch between them at the foot of the sidebar. A llama.cpp address on this computer (`localhost` or `127.0.0.1`, any port) always means Nyx AI Core. Chat never starts or stops Ollama.
- **No silent model swaps:** when a chat's model, Quick ask's or a knowledge base's embedding model isn't installed, Chat says so; it doesn't quietly use another.
- **Import and export:** all chats as one JSON file, and imports from Open WebUI exports.

### Easy on the battery

On hybrid laptops the discrete GPU sleeps when nothing uses it. Chat uses GTK's GL
renderer, which doesn't wake it, and reads NVIDIA memory size with `nvidia-smi` only
while the GPU is already awake.

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/design-nexus/nyx-chat/main/install.sh | bash -s -- chat
```

This builds with Cargo and installs `nyx-chat` to `~/.local/bin`, along with a launcher
entry. Add `-s -- --bind` after `bash` to also print lines that open Quick ask with
Super+Alt+Space:

```lua
o.bind("SUPER + ALT + SPACE", "Quick ask", "chat --quick --selection")
o.window({ title = "^Quick ask$" }, { float = true, center = true, size = { 720, 480 } })
```

To remove it, run the same line with `uninstall.sh` in place of `install.sh`. Add
`-s -- --purge` to also remove its settings and chat history.

Local models need [Nyx AI Core](https://github.com/design-nexus/nyx-ai): install it first. It starts when Chat first needs a model; Chat's Connection settings can stop, start or restart it. Updating Chat retires the llama.cpp server older versions installed. Ollama is optional.
### Coding tools

Nyx AI Core serves an OpenAI-compatible API at `http://127.0.0.1:8080/v1`, the address Chat's server used before. Point a client there, choose a model ID from `GET /v1/models`, and enter any placeholder API key if the client requires one. It binds to localhost and starts on the first request, whether or not Chat is open.

Optional: `wl-clipboard` (Quick ask with the selection), `grim` and `slurp`
(screenshots), `poppler` (PDFs), `geoclue` (desktop location), whisper.cpp and Piper
(voice).

## Usage

```
nyx-chat [--section ID] [--new] [--quick [--selection | --clipboard]] [--screenshot] [--attach FILE…]
```

| Option | What it does |
| --- | --- |
| `--section ID` | Opens `nyx-chat`, `models`, `marketplace`, `arena`, `knowledge`, `prompts`, `stats` or `settings` |
| `--new` | Starts a new chat |
| `--quick` | Opens Quick ask; with `--selection` or `--clipboard`, asks about that text |
| `--screenshot` | Drag out an area of the screen and attach it to a chat |
| `--attach FILE…` | Attaches files to a new message |

| Keys | |
| --- | --- |
| <kbd>Enter</kbd> / <kbd>Shift</kbd>+<kbd>Enter</kbd> | Send / new line (swappable in Settings) |
| <kbd>Esc</kbd> | Stop the reply |
| <kbd>Ctrl</kbd>+<kbd>N</kbd> | New chat |
| <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>N</kbd> | Temporary chat |
| <kbd>Ctrl</kbd>+<kbd>F</kbd> | Search chats (or settings, while Settings is open) |
| <kbd>Ctrl</kbd>+<kbd>Space</kbd> | Dictate |
| <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>S</kbd> | Screenshot into the chat |
| <kbd>Ctrl</kbd>+<kbd>1</kbd>…<kbd>7</kbd> | Go to a page in the sidebar |
| <kbd>Alt</kbd>+<kbd>←</kbd> | Back to the previous page |
| <kbd>Ctrl</kbd>+<kbd>B</kbd> | Collapse or expand the sidebar |
| <kbd>Ctrl</kbd>+<kbd>,</kbd> | Settings |
| <kbd>F1</kbd> | Show the keyboard shortcuts |
| <kbd>Ctrl</kbd>+<kbd>Q</kbd> | Quit |

## Files

| Path | What |
| --- | --- |
| `~/.config/nyx-chat/settings.toml` | Preferences, servers and MCP servers |
| `~/.config/nyx-chat/themes/*.toml` | Your own themes |
| `~/.local/share/nyx-chat/chat.db` | Chats, prompts, memories, knowledge and arena votes (SQLite) |
| `~/.local/share/nyx-chat/attachments/` | Attached images and the text of attached files |

## License

MIT
