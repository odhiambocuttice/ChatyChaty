import { useEffect, useRef, useState, type KeyboardEvent } from "react";
import { toast, Toaster } from "sonner";
import { useTranslation } from "react-i18next";
import { listen } from "@tauri-apps/api/event";
import { platform } from "@tauri-apps/plugin-os";
import { ModelStateEvent, RecordingErrorEvent } from "./lib/types/events";
import "./App.css";
import SecureInputWarning from "./components/SecureInputWarning";
import { ErrorBoundary } from "./components/ErrorBoundary";
import { ModelStatus } from "./components/ModelStatus";
import { PermissionsScreen } from "./components/Permissions";
import {
  AutostartToggle,
  MicrophoneSelector,
  PauseMediaWhileRecording,
  ReplyProviderKeys,
  ShortcutInput,
  ShowOverlay,
  StartHidden,
  TypingSpeed,
} from "./components/settings";
import { SettingsGroup } from "./components/ui/SettingsGroup";
import appIcon from "../src-tauri/icons/128x128@2x.png";
import { usePermissions } from "./hooks/usePermissions";
import { useSettings } from "./hooks/useSettings";
import { getLanguageDirection } from "@/lib/utils/rtl";
import { commands } from "@/bindings";

const TABS = ["shortcuts", "microphone", "basics", "model"] as const;
type Tab = (typeof TABS)[number];

const TAB_LABEL_KEYS: Record<Tab, string> = {
  shortcuts: "settings.tabs.shortcuts",
  microphone: "settings.microphone.title",
  basics: "settings.basics.title",
  model: "settings.tabs.model",
};

function App() {
  const { t, i18n } = useTranslation();
  const [tab, setTab] = useState<Tab>("shortcuts");
  const direction = getLanguageDirection(i18n.language);
  const { refreshAudioDevices } = useSettings();
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
                <SettingsGroup title={t("settings.groups.speechModel")}>
                  <ErrorBoundary context="Model">
                    <ModelStatus />
                  </ErrorBoundary>
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
