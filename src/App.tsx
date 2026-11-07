import { useEffect, useRef, useState, type KeyboardEvent } from "react";
import { toast, Toaster } from "sonner";
import { useTranslation } from "react-i18next";
import { listen } from "@tauri-apps/api/event";
import { platform } from "@tauri-apps/plugin-os";
import { ModelStateEvent, RecordingErrorEvent } from "./lib/types/events";
import "./App.css";
import SecureInputWarning from "./components/SecureInputWarning";
import { LogViewer } from "./components/LogViewer";
import { PermissionsScreen } from "./components/Permissions";
import {
  AutostartToggle,
  ChooseBeforeTyping,
  MicrophoneSelector,
  PasteWithClipboard,
  PauseMediaWhileRecording,
  RemoteReplyPanel,
  ReplyProviderKeys,
  ShortcutInput,
  ShowOverlay,
  StartHidden,
  TypingSpeed,
} from "./components/settings";
import { IconTile, SettingsGroup } from "./components/ui/SettingsGroup";
import { ToggleSwitch } from "./components/ui/ToggleSwitch";
import {
  Bell,
  Keyboard,
  KeyRound,
  Mic,
  MousePointerClick,
  PanelTop,
  PictureInPicture2,
  Power,
  ScrollText,
  Send,
  Settings,
  Type,
  WandSparkles,
} from "lucide-react";
import { getVersion } from "@tauri-apps/api/app";
import AutoreplyIcon from "@/components/icons/AutoreplyIcon";
import appIcon from "../src-tauri/icons/128x128@2x.png";
import { usePermissions } from "./hooks/usePermissions";
import { useSettings } from "./hooks/useSettings";
import { getLanguageDirection } from "@/lib/utils/rtl";
import { commands } from "@/bindings";

/** Shown under the icon in the sidebar; a name, so not translated. */
const APP_NAME = "Chatelle";

const TABS = [
  { id: "buttons", icon: MousePointerClick },
  { id: "shortcuts", icon: Keyboard },
  { id: "microphone", icon: Mic },
  { id: "general", icon: Settings },
  { id: "keys", icon: KeyRound },
  { id: "telegram", icon: Send },
  { id: "logs", icon: ScrollText },
] as const;
type Tab = (typeof TABS)[number]["id"];

function App() {
  const { t, i18n } = useTranslation();
  const [tab, setTab] = useState<Tab>("buttons");
  const [appVersion, setAppVersion] = useState("");
  useEffect(() => {
    getVersion()
      .then(setAppVersion)
      .catch((e) => console.warn("Failed to read the app version:", e));
  }, []);
  const direction = getLanguageDirection(i18n.language);
  const { settings, updateSetting, refreshAudioDevices } = useSettings();
  const permissions = usePermissions();
  const hasInitialized = useRef(false);

  // The window may start hidden; when something needs the user, show it.
  const needsPermissions = permissions.checked && !permissions.allGranted;
  useEffect(() => {
    if (needsPermissions) {
      commands.showMainWindowCommand().catch((e) => {
        console.warn("Failed to show the window for permissions:", e);
      });
    }
  }, [needsPermissions]);

  // Once the permissions are in place: set up typing and shortcuts, and list
  // the microphones (listing them earlier would trigger the OS prompt).
  useEffect(() => {
    if (permissions.allGranted && !hasInitialized.current) {
      hasInitialized.current = true;
      Promise.all([
        commands.initializeEnigo(),
        commands.initializeShortcuts(),
      ]).catch((e) => {
        console.warn("Failed to initialize:", e);
      });
      refreshAudioDevices();
    }
  }, [permissions.allGranted, refreshAudioDevices]);

  // Listen for recording errors from the backend and show a toast
  useEffect(() => {
    const unlisten = listen<RecordingErrorEvent>("recording-error", (event) => {
      const { error_type, detail } = event.payload;

      if (error_type === "microphone_permission_denied") {
        const currentPlatform = platform();
        const platformKey = `errors.micPermissionDenied.${currentPlatform}`;
        const description = t(platformKey, {
          defaultValue: t("errors.micPermissionDenied.generic"),
        });
        toast.error(t("errors.micPermissionDeniedTitle"), { description });
      } else if (error_type === "phone_not_connected") {
        toast.error(t("errors.phoneNotConnected"));
      } else if (error_type === "no_input_device") {
        toast.error(t("errors.noInputDeviceTitle"), {
          description: t("errors.noInputDevice"),
        });
      } else {
        toast.error(
          t("errors.recordingFailed", { error: detail ?? "Unknown error" }),
        );
      }
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, [t]);

  // Listen for typing failures and show a toast. The technical detail is
  // logged to chatelle.log on the Rust side.
  useEffect(() => {
    const unlisten = listen("paste-error", () => {
      toast.error(t("errors.typeFailedTitle"), {
        description: t("errors.typeFailed"),
      });
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, [t]);

  // Listen for transcription failures and show a toast.
  // The payload is the backend error message (also logged to chatelle.log).
  useEffect(() => {
    const unlisten = listen<string>("transcription-error", (event) => {
      toast.error(t("errors.transcriptionFailedTitle"), {
        description: event.payload,
      });
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, [t]);

  // Listen for draft reply failures and show a toast
  useEffect(() => {
    const unlisten = listen<string>("draft-reply-error", (event) => {
      toast.error(t("errors.draftReplyFailedTitle"), {
        description: event.payload,
      });
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, [t]);

  // Listen for watch mode events
  useEffect(() => {
    const unlisteners = [
      listen<string>("watch-error", (event) => {
        toast.error(t("errors.watchErrorTitle"), {
          description: event.payload,
        });
      }),
      listen<boolean>("watch-state-changed", (event) => {
        if (event.payload) {
          toast.success(t("errors.watchStarted"));
        } else {
          toast.info(t("errors.watchStopped"));
        }
      }),
    ];
    return () => {
      unlisteners.forEach((p) => p.then((fn) => fn()));
    };
  }, [t]);

  // Listen for model loading and download failures and show a toast
  useEffect(() => {
    const unlisteners = [
      listen<ModelStateEvent>("model-state-changed", (event) => {
        if (event.payload.event_type === "loading_failed") {
          toast.error(t("errors.modelLoadFailed"), {
            description: event.payload.error,
          });
        }
      }),
      listen<string>("model-download-failed", (event) => {
        toast.error(t("errors.modelDownloadFailed"), {
          description: event.payload,
        });
      }),
    ];
    return () => {
      unlisteners.forEach((p) => p.then((fn) => fn()));
    };
  }, [t]);

  // Up and down arrow keys move between tabs
  const onTabKey = (e: KeyboardEvent<HTMLButtonElement>, id: Tab) => {
    if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
    e.preventDefault();
    const i = TABS.findIndex((tab) => tab.id === id);
    const step = e.key === "ArrowDown" ? 1 : TABS.length - 1;
    const next = TABS[(i + step) % TABS.length].id;
    setTab(next);
    document.getElementById(`tab-${next}`)?.focus();
  };

  // Still checking permissions
  if (!permissions.checked) {
    return null;
  }

  return (
    <>
      <Toaster
        theme="system"
        toastOptions={{
          unstyled: true,
          classNames: {
            toast:
              "bg-background border border-line rounded-lg shadow-lg px-4 py-3 flex items-center gap-3 text-sm",
            title: "font-medium",
            description: "text-ink-3",
          },
        }}
      />
      {needsPermissions ? (
        <PermissionsScreen permissions={permissions} />
      ) : (
        <div
          dir={direction}
          className="h-screen flex bg-sidebar select-none cursor-default"
        >
          <nav
            aria-label={t("settings.tabs.label")}
            className="w-56 shrink-0 h-full overflow-y-auto px-3 pt-5 pb-4"
          >
            <div className="flex items-center gap-2.5 px-2">
              <img src={appIcon} alt="" className="size-10 drop-shadow-sm" />
              <div className="min-w-0">
                <div className="text-row font-semibold tracking-[-0.01em]">
                  {APP_NAME}
                </div>
                <div className="text-[11px] leading-tight text-ink-3">
                  {t("app.version", { version: appVersion })}
                </div>
              </div>
            </div>
            <div
              role="tablist"
              aria-orientation="vertical"
              aria-label={t("settings.tabs.label")}
              className="mt-5 flex flex-col gap-0.5"
            >
              {TABS.map(({ id, icon }) => {
                const on = id === tab;
                return (
                  <button
                    key={id}
                    id={`tab-${id}`}
                    type="button"
                    role="tab"
                    aria-selected={on}
                    aria-controls="settings-panel"
                    tabIndex={on ? 0 : -1}
                    onClick={() => setTab(id)}
                    onKeyDown={(e) => onTabKey(e, id)}
                    className={`flex items-center gap-2.5 w-full min-h-9 px-2 rounded-lg text-start text-body tracking-[-0.01em] cursor-pointer focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-logo-primary ${
                      on
                        ? "bg-logo-primary/20 font-semibold"
                        : "font-medium hover:bg-line"
                    }`}
                  >
                    <IconTile icon={icon} size="sm" />
                    {t(`settings.tabs.${id}`)}
                  </button>
                );
              })}
            </div>
          </nav>
          <main className="flex-1 min-w-0 my-2 me-2 overflow-y-auto rounded-panel border border-line bg-background">
            <div className="max-w-[640px] w-full mx-auto px-6 pt-6 pb-12">
              <h1 className="text-xl font-semibold tracking-[-0.02em]">
                {t(`settings.tabs.${tab}`)}
              </h1>
              <div className="mt-4 empty:hidden">
                <SecureInputWarning />
              </div>
              <div
                id="settings-panel"
                role="tabpanel"
                aria-labelledby={`tab-${tab}`}
                className="mt-5 space-y-3"
              >
                {tab === "buttons" && (
                  <>
                    <SettingsGroup
                      title={t("settings.quickActions.title")}
                      icon={MousePointerClick}
                      defaultOpen
                    >
                      <div className="py-4">
                        <div>
                          <div className="grid grid-cols-2 sm:grid-cols-3 gap-2">
                            <button
                              type="button"
                              onClick={() => commands.triggerDraftReply()}
                              className="flex items-center gap-2 p-2.5 rounded-control border border-line bg-surface hover:border-logo-primary hover:bg-logo-primary/10 transition-all text-left group cursor-pointer"
                            >
                              <AutoreplyIcon className="w-5 h-5 text-logo-primary shrink-0" />
                              <div>
                                <div className="text-xs leading-snug font-semibold text-ink-1 group-hover:text-logo-primary">
                                  {t("settings.quickActions.draft")}
                                </div>
                                <div className="text-[11px] leading-snug text-ink-3 mt-0.5">
                                  {t("settings.quickActions.draftDesc")}
                                </div>
                              </div>
                            </button>

                            <button
                              type="button"
                              onClick={() => commands.triggerTranscribeToggle()}
                              className="flex items-center gap-2 p-2.5 rounded-control border border-line bg-surface hover:border-logo-primary hover:bg-logo-primary/10 transition-all text-left group cursor-pointer"
                            >
                              <Mic className="w-5 h-5 text-logo-primary shrink-0" />
                              <div>
                                <div className="text-xs leading-snug font-semibold text-ink-1 group-hover:text-logo-primary">
                                  {t("settings.quickActions.dictate")}
                                </div>
                                <div className="text-[11px] leading-snug text-ink-3 mt-0.5">
                                  {t("settings.quickActions.dictateDesc")}
                                </div>
                              </div>
                            </button>

                            <button
                              type="button"
                              onClick={() =>
                                commands.triggerDraftReplyHintStart()
                              }
                              className="flex items-center gap-2 p-2.5 rounded-control border border-line bg-surface hover:border-logo-primary hover:bg-logo-primary/10 transition-all text-left group cursor-pointer"
                            >
                              <WandSparkles className="w-5 h-5 text-logo-primary shrink-0" />
                              <div>
                                <div className="text-xs leading-snug font-semibold text-ink-1 group-hover:text-logo-primary">
                                  {t("settings.quickActions.hint")}
                                </div>
                                <div className="text-[11px] leading-snug text-ink-3 mt-0.5">
                                  {t("settings.quickActions.hintDesc")}
                                </div>
                              </div>
                            </button>

                            <button
                              type="button"
                              onClick={() => commands.toggleWatchSession()}
                              className="flex items-center gap-2 p-2.5 rounded-control border border-line bg-surface hover:border-logo-primary hover:bg-logo-primary/10 transition-all text-left group cursor-pointer"
                            >
                              <Bell className="w-5 h-5 text-logo-primary shrink-0" />
                              <div>
                                <div className="text-xs leading-snug font-semibold text-ink-1 group-hover:text-logo-primary">
                                  {t("settings.quickActions.watch")}
                                </div>
                                <div className="text-[11px] leading-snug text-ink-3 mt-0.5">
                                  {t("settings.quickActions.watchDesc")}
                                </div>
                              </div>
                            </button>
                          </div>
                        </div>
                      </div>
                    </SettingsGroup>
                    <SettingsGroup
                      title={t("settings.groups.buttonOptions")}
                      icon={PanelTop}
                      defaultOpen
                    >
                      <ToggleSwitch
                        checked={settings?.show_floating_dock ?? true}
                        onChange={(val) => {
                          commands.changeShowFloatingDockSetting(val);
                          updateSetting("show_floating_dock", val);
                        }}
                        label={t("settings.floatingDock.title")}
                        description={t("settings.floatingDock.description")}
                        descriptionMode="inline"
                        grouped={true}
                      />
                      <ToggleSwitch
                        checked={settings?.auto_focus_input ?? true}
                        onChange={(val) => {
                          commands.changeAutoFocusInputSetting(val);
                          updateSetting("auto_focus_input", val);
                        }}
                        label={t("settings.autoFocusInput.title")}
                        description={t("settings.autoFocusInput.description")}
                        descriptionMode="inline"
                        grouped={true}
                      />
                    </SettingsGroup>
                  </>
                )}
                {tab === "shortcuts" && (
                  <SettingsGroup
                    title={t("settings.groups.keyboardShortcuts")}
                    icon={Keyboard}
                    defaultOpen
                  >
                    <ShortcutInput
                      shortcutId="draft_reply"
                      descriptionMode="inline"
                      grouped={true}
                    />
                    <ShortcutInput
                      shortcutId="transcribe"
                      descriptionMode="inline"
                      grouped={true}
                    />
                    <ShortcutInput
                      shortcutId="draft_reply_hint"
                      descriptionMode="inline"
                      grouped={true}
                    />
                    <ShortcutInput
                      shortcutId="watch_chat"
                      descriptionMode="inline"
                      grouped={true}
                    />
                  </SettingsGroup>
                )}
                {tab === "telegram" && (
                  <SettingsGroup
                    title={t("settings.remoteReply.title")}
                    icon={Send}
                    defaultOpen
                  >
                    <RemoteReplyPanel />
                  </SettingsGroup>
                )}
                {tab === "keys" && (
                  <SettingsGroup
                    title={t("settings.replyKeys.title")}
                    icon={KeyRound}
                    defaultOpen
                  >
                    <ReplyProviderKeys />
                  </SettingsGroup>
                )}
                {tab === "microphone" && (
                  <SettingsGroup
                    title={t("settings.groups.input")}
                    icon={Mic}
                    defaultOpen
                  >
                    <MicrophoneSelector
                      descriptionMode="inline"
                      grouped={true}
                    />
                  </SettingsGroup>
                )}
                {tab === "general" && (
                  <>
                    <SettingsGroup
                      title={t("settings.groups.recording")}
                      icon={Type}
                      defaultOpen
                    >
                      <PauseMediaWhileRecording
                        descriptionMode="tooltip"
                        grouped={true}
                      />
                      <TypingSpeed descriptionMode="tooltip" grouped={true} />
                      <ChooseBeforeTyping
                        descriptionMode="tooltip"
                        grouped={true}
                      />
                      <PasteWithClipboard
                        descriptionMode="tooltip"
                        grouped={true}
                      />
                    </SettingsGroup>
                    <SettingsGroup
                      title={t("settings.groups.overlay")}
                      icon={PictureInPicture2}
                    >
                      <ShowOverlay descriptionMode="tooltip" grouped={true} />
                    </SettingsGroup>
                    <SettingsGroup
                      title={t("settings.groups.startup")}
                      icon={Power}
                    >
                      <StartHidden descriptionMode="tooltip" grouped={true} />
                      <AutostartToggle
                        descriptionMode="tooltip"
                        grouped={true}
                      />
                    </SettingsGroup>
                  </>
                )}
                {tab === "logs" && (
                  <SettingsGroup
                    title={t("settings.groups.log")}
                    icon={ScrollText}
                    defaultOpen
                  >
                    <LogViewer />
                  </SettingsGroup>
                )}
              </div>
            </div>
          </main>
        </div>
      )}
    </>
  );
}

export default App;
