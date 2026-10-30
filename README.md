# Handy

Press a shortcut, speak, and your words are typed where your cursor is. Everything runs on your own computer, and nothing is uploaded.

Handy types the text one key press at a time, like a person, so it also works in text fields that block pasting.

## Install

The installers are built by GitHub Actions. There is no release page, so you download them from the build.

1. Open the [Actions page](https://github.com/odhiambocuttice/Handy/actions) and click the latest green **Main Branch Build**. You need to be logged in to GitHub.
2. Under **Artifacts**, download the one for your computer and unzip it:

   | Computer                 | Artifact                                      |
   | ------------------------ | --------------------------------------------- |
   | Mac, Apple Silicon (M1+) | `handy-aarch64-apple-darwin`                  |
   | Mac, Intel               | `handy-x86_64-apple-darwin`                   |
   | Windows                  | `handy-x86_64-pc-windows-msvc`                |
   | Windows on ARM           | `handy-aarch64-pc-windows-msvc`               |
   | Linux, AppImage or rpm   | `handy-ubuntu-24.04-x86_64-unknown-linux-gnu` |
   | Linux, deb               | `handy-ubuntu-22.04-x86_64-unknown-linux-gnu` |

3. Install it. These builds are not signed, so your computer will warn you the first time:
   - **Mac:** drag `Handy.app` into Applications. In Terminal, run `xattr -cr /Applications/Handy.app`, then right-click the app and choose **Open**.
   - **Windows:** run the `.exe` installer from the `nsis` folder. When SmartScreen says the app is unrecognised, click **More info**, then **Run anyway**.
   - **Linux:** make the AppImage executable and run it, or install the `.deb` or `.rpm`.

## First launch

1. Allow the **microphone**. On a Mac, also allow **Accessibility**, which is what lets Handy type for you.
2. Wait for the speech model to download once (about 730 MB). The settings window shows the progress, and the model is loaded each time Handy starts.
3. Click into any text field, press the shortcut, and talk.

## Use it

- **Shortcut:** `Option+Space` on a Mac, `Ctrl+Space` on Windows and Linux. Hold it while you speak, or tap it to start and tap again to stop. You can change it in the settings window.
- **Cancel:** press `Escape` while recording.
- **Settings:** click the tray icon and choose **Settings**. You can pick the microphone, change the shortcut, and set the overlay, start hidden and launch on startup.

Handy transcribes English with [Parakeet Unified English 0.6B](https://huggingface.co/handy-computer/parakeet-unified-en-0.6b-gguf).

## Troubleshooting

- **Nothing is typed (Mac):** check System Settings → Privacy & Security → Accessibility and make sure Handy is switched on. If you changed the app, switch it off and on again.
- **The model will not download (proxy or firewall):** download [this file](https://huggingface.co/handy-computer/parakeet-unified-en-0.6b-gguf/resolve/main/parakeet-unified-en-0.6b-Q8_0.gguf) in a browser without renaming it. Put it in the `models` folder inside Handy's data folder (`~/Library/Application Support/com.pais.handy/` on a Mac, `%APPDATA%\com.pais.handy\` on Windows, `~/.config/com.pais.handy/` on Linux). Then restart Handy.
- **Logs:** `~/Library/Logs/com.pais.handy/handy.log` on a Mac.

## Build from source

See [BUILD.md](BUILD.md).

## License

MIT. Handy was started as [cjpais/Handy](https://github.com/cjpais/Handy); this fork keeps only the single-model dictation.
