import { useEffect, useRef, useState, type KeyboardEvent } from "react";
import { toast, Toaster } from "sonner";
import { useTranslation } from "react-i18next";
import { listen } from "@tauri-apps/api/event";
import { platform } from "@tauri-apps/plugin-os";
import { ModelStateEvent, RecordingErrorEvent } from "./lib/types/events";
import "./App.css";
import SecureInputWarning from "./components/SecureInputWarning";
import { ErrorBoundary } from "./components/ErrorBoundary";
import { LogViewer } from "./components/LogViewer";
import { ModelStatus } from "./components/ModelStatus";
import { PermissionsScreen } from "./components/Permissions";
import {
  AutostartToggle,
  MicrophoneSelector,
  PauseMediaWhileRecording,
  RemoteReplyPanel,
  ReplyProviderKeys,
  ShortcutInput,
  ShowOverlay,
  SpeechModelPicker,
  StartHidden,
  TypingSpeed,
} from "./components/settings";
import { SettingsGroup } from "./components/ui/SettingsGroup";
import { ToggleSwitch } from "./components/ui/ToggleSwitch";
import { Sparkles, Mic, RefreshCw, Radio, Eye } from "lucide-react";
import appIcon from "../src-tauri/icons/128x128@2x.png";
import { usePermissions } from "./hooks/usePermissions";
import { useSettings } from "./hooks/useSettings";
import { getLanguageDirection } from "@/lib/utils/rtl";
import { commands } from "@/bindings";

const TABS = ["shortcuts", "microphone", "basics", "model", "logs"] as const;
type Tab = (typeof TABS)[number];

const TAB_LABEL_KEYS: Record<Tab, string> = {
  shortcuts: "settings.tabs.shortcuts",
  microphone: "settings.microphone.title",
  basics: "settings.basics.title",
  model: "settings.tabs.model",
  logs: "settings.tabs.logs",
};

function App() {
  const { t, i18n } = useTranslation();
  const [tab, setTab] = useState<Tab>("shortcuts");
  const direction = getLanguageDirection(i18n.language);
  const { settings, updateSetting, refreshAudioDevices } = useSettings();
  const permissions = usePermissions();
  const hasInitialized = useRef(false);

  // Open on the Model tab while the model is not downloaded, so a first-run
  // download shows its progress.
  useEffect(() => {
    commands
      .getModelStatus()
      .then((status) => {
        if (!status.is_downloaded) setTab("model");
      })
      .catch((e) => console.warn("Failed to read the model status:", e));
  }, []);

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
  // logged to handy.log on the Rust side.
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
  // The payload is the backend error message (also logged to handy.log).
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

  // Arrow keys move between tabs (mirrored for right-to-left languages)
  const onTabKey = (e: KeyboardEvent<HTMLButtonElement>, id: Tab) => {
    const forward = direction === "rtl" ? "ArrowLeft" : "ArrowRight";
    const back = direction === "rtl" ? "ArrowRight" : "ArrowLeft";
    if (e.key !== forward && e.key !== back) return;
    e.preventDefault();
    const i = TABS.indexOf(id);
    const next =
      TABS[(i + (e.key === forward ? 1 : TABS.length - 1)) % TABS.length];
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
          className="h-screen overflow-y-auto select-none cursor-default"
        >
          <div className="max-w-[720px] w-full mx-auto px-6 pt-10 pb-12">
            <header className="text-center">
              <img
                src={appIcon}
                alt=""
                className="size-16 mx-auto drop-shadow-sm"
              />
            </header>
            <div className="mt-6 empty:hidden">
              <SecureInputWarning />
            </div>
            <div className="flex justify-center mt-8">
              <div
                role="tablist"
                aria-label={t("settings.tabs.label")}
                className="flex flex-wrap justify-center gap-0.5 p-[3px] bg-wash rounded-full"
              >
                {TABS.map((id) => {
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
                      className={`min-h-9 px-5 rounded-full text-body tracking-[-0.01em] cursor-pointer focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-logo-primary ${
                        on
                          ? "bg-text text-background font-semibold"
                          : "font-medium hover:bg-line"
                      }`}
                    >
                      {t(TAB_LABEL_KEYS[id])}
                    </button>
                  );
                })}
              </div>
            </div>
            <div
              id="settings-panel"
              role="tabpanel"
              aria-labelledby={`tab-${tab}`}
              className="mt-9 min-h-80 space-y-7"
            >
              {tab === "shortcuts" && (
                <>
                  <SettingsGroup title={t("settings.groups.quickActions")}>
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
                    <div className="p-3.5 bg-background border border-line rounded-control mt-2">
                      <div className="text-xs font-semibold uppercase tracking-wider text-ink-3 mb-2.5">
                        {t("settings.quickActions.title")}
                      </div>
                      <div className="grid grid-cols-2 sm:grid-cols-3 gap-2">
                        <button
                          type="button"
                          onClick={() => commands.triggerDraftReply()}
                          className="flex items-center gap-2 p-2.5 rounded-control border border-line bg-surface hover:border-logo-primary hover:bg-logo-primary/10 transition-all text-left group cursor-pointer"
                        >
                          <Sparkles className="w-5 h-5 text-logo-primary shrink-0" />
                          <div>
                            <div className="text-xs font-semibold text-ink-1 group-hover:text-logo-primary">
                              {t("settings.quickActions.draft")}
                            </div>
                            <div className="text-[11px] text-ink-3">
                              {t("settings.quickActions.draftDesc")}
                            </div>
                          </div>
                        </button>

                        <button
                          type="button"
                          onClick={() => commands.triggerDraftReplyHintStart()}
                          className="flex items-center gap-2 p-2.5 rounded-control border border-line bg-surface hover:border-logo-primary hover:bg-logo-primary/10 transition-all text-left group cursor-pointer"
                        >
                          <Mic className="w-5 h-5 text-purple-400 shrink-0" />
                          <div>
                            <div className="text-xs font-semibold text-ink-1 group-hover:text-logo-primary">
                              {t("settings.quickActions.hint")}
                            </div>
                            <div className="text-[11px] text-ink-3">
                              {t("settings.quickActions.hintDesc")}
                            </div>
                          </div>
                        </button>

                        <button
                          type="button"
                          onClick={() => commands.triggerRegenerateReply()}
                          className="flex items-center gap-2 p-2.5 rounded-control border border-line bg-surface hover:border-logo-primary hover:bg-logo-primary/10 transition-all text-left group cursor-pointer"
                        >
                          <RefreshCw className="w-5 h-5 text-blue-400 shrink-0" />
                          <div>
                            <div className="text-xs font-semibold text-ink-1 group-hover:text-logo-primary">
                              {t("settings.quickActions.reroll")}
                            </div>
                            <div className="text-[11px] text-ink-3">
                              {t("settings.quickActions.rerollDesc")}
                            </div>
                          </div>
                        </button>

                        <button
                          type="button"
                          onClick={() => commands.triggerTranscribeToggle()}
                          className="flex items-center gap-2 p-2.5 rounded-control border border-line bg-surface hover:border-logo-primary hover:bg-logo-primary/10 transition-all text-left group cursor-pointer"
                        >
                          <Radio className="w-5 h-5 text-amber-400 shrink-0" />
                          <div>
                            <div className="text-xs font-semibold text-ink-1 group-hover:text-logo-primary">
                              {t("settings.quickActions.dictate")}
                            </div>
                            <div className="text-[11px] text-ink-3">
                              {t("settings.quickActions.dictateDesc")}
                            </div>
                          </div>
                        </button>

                        <button
                          type="button"
                          onClick={() => commands.toggleWatchSession()}
                          className="flex items-center gap-2 p-2.5 rounded-control border border-line bg-surface hover:border-logo-primary hover:bg-logo-primary/10 transition-all text-left group cursor-pointer"
                        >
                          <Eye className="w-5 h-5 text-emerald-400 shrink-0" />
                          <div>
                            <div className="text-xs font-semibold text-ink-1 group-hover:text-logo-primary">
                              {t("settings.quickActions.watch")}
                            </div>
                            <div className="text-[11px] text-ink-3">
                              {t("settings.quickActions.watchDesc")}
                            </div>
                          </div>
                        </button>
                      </div>
                    </div>
                  </SettingsGroup>
                  <SettingsGroup title={t("settings.groups.keyboardShortcuts")}>
                    <ShortcutInput
                      shortcutId="transcribe"
                      descriptionMode="inline"
                      grouped={true}
                    />
                    <ShortcutInput
                      shortcutId="draft_reply"
                      descriptionMode="inline"
                      grouped={true}
                    />
                    <ShortcutInput
                      shortcutId="draft_reply_hint"
                      descriptionMode="inline"
                      grouped={true}
                    />
                    <ShortcutInput
                      shortcutId="regenerate_reply"
                      descriptionMode="inline"
                      grouped={true}
                    />
                    <ShortcutInput
                      shortcutId="watch_chat"
                      descriptionMode="inline"
                      grouped={true}
                    />
                  </SettingsGroup>
                  <SettingsGroup title={t("settings.remoteReply.title")}>
                    <RemoteReplyPanel />
                  </SettingsGroup>
                  <SettingsGroup title={t("settings.replyKeys.title")}>
                    <ReplyProviderKeys />
                  </SettingsGroup>
                </>
              )}
              {tab === "microphone" && (
                <SettingsGroup title={t("settings.groups.input")}>
                  <MicrophoneSelector descriptionMode="inline" grouped={true} />
                </SettingsGroup>
              )}
              {tab === "basics" && (
                <>
                  <SettingsGroup title={t("settings.groups.recording")}>
                    <PauseMediaWhileRecording
                      descriptionMode="tooltip"
                      grouped={true}
                    />
                    <TypingSpeed descriptionMode="tooltip" grouped={true} />
                  </SettingsGroup>
                  <SettingsGroup title={t("settings.groups.overlay")}>
                    <ShowOverlay descriptionMode="tooltip" grouped={true} />
                  </SettingsGroup>
                  <SettingsGroup title={t("settings.groups.startup")}>
                    <StartHidden descriptionMode="tooltip" grouped={true} />
                    <AutostartToggle descriptionMode="tooltip" grouped={true} />
                  </SettingsGroup>
                </>
              )}
              {tab === "model" && (
                <>
                  <SettingsGroup title={t("settings.groups.speechModel")}>
                    <SpeechModelPicker />
                  </SettingsGroup>
                  <SettingsGroup title={t("settings.groups.modelStatus")}>
                    <ErrorBoundary context="Model">
                      <ModelStatus />
                    </ErrorBoundary>
                  </SettingsGroup>
                </>
              )}
              {tab === "logs" && (
                <SettingsGroup title={t("settings.groups.log")}>
                  <LogViewer />
                </SettingsGroup>
              )}
            </div>
          </div>
        </div>
      )}
    </>
  );
}

export default App;
