import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { platform } from "@tauri-apps/plugin-os";
import { getCurrentWindow } from "@tauri-apps/api/window";
import {
  checkAccessibilityPermission,
  checkMicrophonePermission,
  requestAccessibilityPermission,
  requestMicrophonePermission,
} from "tauri-plugin-macos-permissions-api";
import { toast } from "sonner";
import { commands } from "@/bindings";
import { useSettingsStore } from "@/stores/settingsStore";

export type PermissionStatus = "checking" | "needed" | "waiting" | "granted";
export type PermissionPlatform = "macos" | "windows" | "other";

export interface Permissions {
  platform: PermissionPlatform;
  microphone: PermissionStatus;
  accessibility: PermissionStatus;
  /** The first check is done. */
  checked: boolean;
  allGranted: boolean;
  /** Asked once already: asking again opens the OS settings instead. */
  microphoneAsked: boolean;
  requestMicrophone: () => Promise<void>;
  requestAccessibility: () => Promise<void>;
}

const POLL_INTERVAL_MS = 1000;
// No answer seen this long after asking: offer the button again.
const WAITING_TIMEOUT_MS = 15_000;
// Back in front: give macOS a moment to record the answer before reading it.
const FOCUS_SETTLE_MS = 500;

type PermissionRowName = "microphone" | "accessibility";

const currentPlatform = (): PermissionPlatform => {
  const p = platform();
  return p === "macos" ? "macos" : p === "windows" ? "windows" : "other";
};

/** What the OS says right now: microphone (macOS, Windows) and accessibility (macOS). */
const readPermissions = async (
  os: PermissionPlatform,
): Promise<{ microphone: boolean; accessibility: boolean }> => {
  if (os === "macos") {
    const [accessibility, microphone] = await Promise.all([
      checkAccessibilityPermission(),
      checkMicrophonePermission(),
    ]);
    return { microphone, accessibility };
  }
  if (os === "windows") {
    const status = await commands.getWindowsMicrophonePermissionStatus();
    return {
      microphone: !status.supported || status.overall_access !== "denied",
      accessibility: true,
    };
  }
  return { microphone: true, accessibility: true };
};

/**
 * While a row waits: recheck it when Chatelle comes back to the front, and once
 * after WAITING_TIMEOUT_MS if Chatelle is in front then (away in Settings, the
 * row keeps waiting).
 */
const useRecheckWhileWaiting = (
  waiting: boolean,
  recheck: () => void,
): void => {
  useEffect(() => {
    if (!waiting) return;
    let settle: ReturnType<typeof setTimeout> | undefined;
    const unlisten = getCurrentWindow().onFocusChanged(({ payload }) => {
      if (!payload) return;
      clearTimeout(settle);
      settle = setTimeout(recheck, FOCUS_SETTLE_MS);
    });
    const timer = setTimeout(() => {
      if (document.hasFocus()) recheck();
    }, WAITING_TIMEOUT_MS);
    return () => {
      clearTimeout(settle);
      clearTimeout(timer);
      unlisten.then((off) => off());
    };
  }, [waiting, recheck]);
};

/**
 * Microphone and accessibility permissions: check, request, then poll until
 * granted. Granting accessibility initializes typing and shortcuts, so no
 * relaunch is needed.
 */
export const usePermissions = (): Permissions => {
  const { t } = useTranslation();
  const refreshAudioDevices = useSettingsStore((s) => s.refreshAudioDevices);
  const [os] = useState(currentPlatform);
  const [microphone, setMicrophone] = useState<PermissionStatus>("checking");
  const [accessibility, setAccessibility] =
    useState<PermissionStatus>("checking");
  const [microphoneAsked, setMicrophoneAsked] = useState(false);
  const pollingRef = useRef<ReturnType<typeof setInterval> | null>(null);

  const stopPolling = useCallback(() => {
    if (pollingRef.current) {
      clearInterval(pollingRef.current);
      pollingRef.current = null;
    }
  }, []);

  const apply = useCallback(
    (granted: { microphone: boolean; accessibility: boolean }) => {
      setMicrophone((prev) =>
        granted.microphone ? "granted" : prev === "waiting" ? prev : "needed",
      );
      setAccessibility((prev) => {
        if (granted.accessibility && prev !== "granted") {
          Promise.all([
            commands.initializeEnigo(),
            commands.initializeShortcuts(),
          ]).catch((e) => console.warn("Failed to initialize input:", e));
        }
        return granted.accessibility
          ? "granted"
          : prev === "waiting"
            ? prev
            : "needed";
      });
      return granted.microphone && granted.accessibility;
    },
    [],
  );

  // Allowed while Chatelle runs: list the microphones now. At launch, App
  // lists them once everything is allowed.
  const previousMicrophone = useRef(microphone);
  useEffect(() => {
    const previous = previousMicrophone.current;
    previousMicrophone.current = microphone;
    if (
      microphone === "granted" &&
      previous !== "granted" &&
      previous !== "checking"
    ) {
      refreshAudioDevices().catch((e) =>
        console.warn("Failed to refresh audio devices:", e),
      );
    }
  }, [microphone, refreshAudioDevices]);

  // A waiting row whose answer was no (or never came): offer the button again.
  const recheckWaiting = useCallback(
    async (row: PermissionRowName) => {
      try {
        const granted = (await readPermissions(os))[row];
        const setRow = row === "microphone" ? setMicrophone : setAccessibility;
        setRow((prev) =>
          prev === "waiting" ? (granted ? "granted" : "needed") : prev,
        );
      } catch (error) {
        console.warn("Failed to recheck permissions:", error);
      }
    },
    [os],
  );
  const recheckMicrophone = useCallback(
    () => recheckWaiting("microphone"),
    [recheckWaiting],
  );
  const recheckAccessibility = useCallback(
    () => recheckWaiting("accessibility"),
    [recheckWaiting],
  );

  const startPolling = useCallback(() => {
    if (pollingRef.current) return;
    pollingRef.current = setInterval(async () => {
      try {
        if (apply(await readPermissions(os))) stopPolling();
      } catch (error) {
        console.error("Error checking permissions:", error);
        stopPolling();
        toast.error(t("permissions.errors.checkFailed"));
      }
    }, POLL_INTERVAL_MS);
  }, [apply, os, stopPolling, t]);

  // The first check.
  useEffect(() => {
    readPermissions(os)
      .then(apply)
      .catch((error) => {
        console.error("Failed to check permissions:", error);
        toast.error(t("permissions.errors.checkFailed"));
        setMicrophone("needed");
        setAccessibility("needed");
      });
    // Asked in an earlier run: the button opens System Settings.
    if (os === "macos") {
      commands
        .getMacosMicrophoneStatus()
        .then((status) => setMicrophoneAsked(status !== "not_determined"))
        .catch((e) => console.warn("Failed to read microphone status:", e));
    }
    return stopPolling;
    // The first check runs once.
  }, []);

  useRecheckWhileWaiting(microphone === "waiting", recheckMicrophone);
  useRecheckWhileWaiting(accessibility === "waiting", recheckAccessibility);

  const requestAccessibility = async () => {
    try {
      await requestAccessibilityPermission();
      setAccessibility("waiting");
      startPolling();
    } catch (error) {
      console.error("Failed to request accessibility permission:", error);
      toast.error(t("permissions.errors.requestFailed"));
    }
  };

  const requestMicrophone = async () => {
    try {
      // macOS prompts only while it has no answer; after that, only System
      // Settings can allow it.
      const canPrompt =
        os === "macos" &&
        (await commands.getMacosMicrophoneStatus()) === "not_determined";
      if (canPrompt) {
        await requestMicrophonePermission();
      } else {
        await commands.openMicrophonePrivacySettings();
      }
      setMicrophoneAsked(true);
      setMicrophone("waiting");
      startPolling();
    } catch (error) {
      console.error("Failed to request microphone permission:", error);
      toast.error(t("permissions.errors.requestFailed"));
    }
  };

  return {
    platform: os,
    microphone,
    accessibility,
    checked: microphone !== "checking" && accessibility !== "checking",
    allGranted: microphone === "granted" && accessibility === "granted",
    microphoneAsked,
    requestMicrophone,
    requestAccessibility,
  };
};
