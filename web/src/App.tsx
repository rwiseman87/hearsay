import { lazy, Suspense, useRef, useState } from "react";

import { useMeetings, useSetup, useStatus } from "./api/hooks";
import type { MeetingRead } from "./api/types";
import { Dashboard } from "./components/Dashboard";
import { Library } from "./components/Library";
import { LiveRecording } from "./components/LiveRecording";
import { NavRail } from "./components/NavRail";
import { RecordMenu } from "./components/RecordMenu";
import { SearchBox } from "./components/SearchBox";
import { SetupScreen } from "./components/SetupScreen";
import { TranscriptView } from "./components/TranscriptView";

// Route-level code splitting: the settings page loads only when opened.
const SettingsPage = lazy(() => import("./components/SettingsPage"));

export function App() {
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [showSettings, setShowSettings] = useState(false);
  const [showSearch, setShowSearch] = useState(false);
  const [showRecord, setShowRecord] = useState(false);
  // The Library (meetings browser) is a full main-area view, shown when no meeting is open.
  const [library, setLibrary] = useState(false);
  const meetings = useMeetings();
  const items = meetings.data?.items ?? [];
  const selected: MeetingRead | null = items.find((m) => m.id === selectedId) ?? null;
  const recording = items.some((m) => m.status === "recording");
  // Poll live-engine readiness from app launch (not just when the record popover opens), so the
  // sidecars' model load overlaps with browsing and the popover reflects readiness immediately. An
  // errored probe reads as ready so a status-endpoint problem never bricks Start.
  const status = useStatus();
  const sidecarsReady = status.isError || (status.data?.sidecars_ready ?? false);

  // A pending "jump to this moment" from a global search result. The nonce makes each jump distinct
  // so repeated jumps to the same meeting/time re-trigger the scroll in TranscriptView.
  const [jump, setJump] = useState<{ meetingId: string; startS: number; nonce: number } | null>(null);
  const jumpNonce = useRef(0);
  const onJump = (meetingId: string, startS: number) => {
    setSelectedId(meetingId);
    setShowSearch(false);
    jumpNonce.current += 1;
    setJump({ meetingId, startS, nonce: jumpNonce.current });
  };

  // Without models there is nothing to record with, so setup replaces the app rather than sitting
  // behind it. An errored probe reads as ready: a status-endpoint problem must not lock anyone out.
  const setup = useSetup();
  if (setup.isLoading) return null; // one loopback request; holding a frame beats flashing the app
  if (setup.data?.required) return <SetupScreen />;

  return (
    <div className="app">
      <main className="app__main">
        <NavRail
          onOpenSettings={() => setShowSettings(true)}
          onGoHome={() => {
            setSelectedId(null);
            setLibrary(false);
          }}
          onOpenRecord={() => setShowRecord(true)}
          recordActive={showRecord}
          onOpenMeetings={() => {
            setSelectedId(null);
            setLibrary(true);
          }}
          meetingsActive={library && selected == null}
          onOpenSearch={() => setShowSearch(true)}
          searchActive={showSearch}
        />
        {selected == null && library ? (
          <Library
            meetings={items}
            isLoading={meetings.isLoading}
            error={meetings.error}
            onSelect={setSelectedId}
          />
        ) : selected == null ? (
          <Dashboard
            meetings={items}
            isLoading={meetings.isLoading}
            error={meetings.error}
            onSelect={setSelectedId}
            onJump={onJump}
          />
        ) : selected.status === "recording" ? (
          <LiveRecording meeting={selected} />
        ) : (
          <TranscriptView
            meeting={selected}
            jumpTo={
              jump && jump.meetingId === selectedId ? { startS: jump.startS, nonce: jump.nonce } : null
            }
          />
        )}
      </main>
      {showRecord ? (
        <div className="record-overlay" onMouseDown={() => setShowRecord(false)}>
          <div className="record-overlay__panel" onMouseDown={(event) => event.stopPropagation()}>
            <RecordMenu
              recording={recording}
              sidecarsReady={sidecarsReady}
              onStarted={(id) => {
                setShowRecord(false);
                setSelectedId(id);
              }}
              onClose={() => setShowRecord(false)}
            />
          </div>
        </div>
      ) : null}
      {showSearch ? (
        <div className="search-overlay" onMouseDown={() => setShowSearch(false)}>
          <div className="search-overlay__panel" onMouseDown={(event) => event.stopPropagation()}>
            <SearchBox onJump={onJump} onClose={() => setShowSearch(false)} autoFocus />
          </div>
        </div>
      ) : null}
      {showSettings ? (
        <Suspense fallback={null}>
          <SettingsPage onClose={() => setShowSettings(false)} />
        </Suspense>
      ) : null}
    </div>
  );
}
