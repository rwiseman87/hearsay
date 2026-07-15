import {
  lazy,
  Suspense,
  useEffect,
  useRef,
  useState,
  type CSSProperties,
  type KeyboardEvent as ReactKeyboardEvent,
  type PointerEvent as ReactPointerEvent,
} from "react";

import { useMeetings } from "./api/hooks";
import type { MeetingRead } from "./api/types";
import { MeetingList } from "./components/MeetingList";
import { TranscriptView } from "./components/TranscriptView";

// Route-level code splitting: the settings page loads only when opened.
const SettingsPage = lazy(() => import("./components/SettingsPage"));

const SIDEBAR_MIN = 180;
const SIDEBAR_MAX = 560;
const SIDEBAR_DEFAULT = 280;
const SIDEBAR_KEY = "hearsay.sidebarWidth";

const clampSidebar = (value: number) =>
  Math.min(Math.max(value, SIDEBAR_MIN), SIDEBAR_MAX);

// Sidebar width, persisted across sessions in localStorage so a resize sticks.
function useSidebarWidth() {
  const [width, setWidth] = useState(() => {
    const stored = Number(localStorage.getItem(SIDEBAR_KEY));
    return Number.isFinite(stored) && stored > 0 ? clampSidebar(stored) : SIDEBAR_DEFAULT;
  });
  useEffect(() => {
    localStorage.setItem(SIDEBAR_KEY, String(width));
  }, [width]);
  return [width, setWidth] as const;
}

export function App() {
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [showSettings, setShowSettings] = useState(false);
  const [sidebarWidth, setSidebarWidth] = useSidebarWidth();
  const drag = useRef<{ startX: number; startWidth: number } | null>(null);
  const meetings = useMeetings();
  const items = meetings.data?.items ?? [];
  const selected: MeetingRead | null = items.find((m) => m.id === selectedId) ?? null;

  const onResizeStart = (event: ReactPointerEvent<HTMLDivElement>) => {
    event.preventDefault();
    drag.current = { startX: event.clientX, startWidth: sidebarWidth };
    event.currentTarget.setPointerCapture(event.pointerId);
  };
  const onResizeMove = (event: ReactPointerEvent<HTMLDivElement>) => {
    if (!drag.current) return;
    setSidebarWidth(clampSidebar(drag.current.startWidth + (event.clientX - drag.current.startX)));
  };
  const onResizeEnd = (event: ReactPointerEvent<HTMLDivElement>) => {
    drag.current = null;
    event.currentTarget.releasePointerCapture(event.pointerId);
  };
  // Keyboard resize for the separator (arrow keys nudge the width).
  const onResizeKey = (event: ReactKeyboardEvent<HTMLDivElement>) => {
    if (event.key === "ArrowLeft") setSidebarWidth((w) => clampSidebar(w - 16));
    else if (event.key === "ArrowRight") setSidebarWidth((w) => clampSidebar(w + 16));
  };

  return (
    <div className="app">
      <header className="app__bar">
        <h1 className="app__title">Hearsay - It's what happened, probably</h1>
        <button type="button" className="app__settings" onClick={() => setShowSettings(true)}>
          Settings
        </button>
      </header>
      <main
        className="app__main"
        style={{ "--sidebar-width": `${sidebarWidth}px` } as CSSProperties}
      >
        <MeetingList
          meetings={items}
          isLoading={meetings.isLoading}
          error={meetings.error}
          selectedId={selectedId}
          onSelect={setSelectedId}
        />
        <div
          className="app__resizer"
          role="separator"
          aria-orientation="vertical"
          aria-label="Resize sidebar"
          aria-valuenow={sidebarWidth}
          aria-valuemin={SIDEBAR_MIN}
          aria-valuemax={SIDEBAR_MAX}
          tabIndex={0}
          onPointerDown={onResizeStart}
          onPointerMove={onResizeMove}
          onPointerUp={onResizeEnd}
          onKeyDown={onResizeKey}
        />
        <TranscriptView meeting={selected} />
      </main>
      {showSettings ? (
        <Suspense fallback={null}>
          <SettingsPage onClose={() => setShowSettings(false)} />
        </Suspense>
      ) : null}
    </div>
  );
}
