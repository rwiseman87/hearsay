interface Props {
  // Whether the meeting-list panel is currently revealed beside the rail.
  meetingsOpen: boolean;
  onToggleMeetings: () => void;
  onOpenSettings: () => void;
}

// The slim, always-present left icon rail. Its toggle reveals or collapses the meeting-list panel
// (giving the transcript more room while recording); the gear opens Settings. Branding pins the top,
// the gear pins the bottom.
export function NavRail({ meetingsOpen, onToggleMeetings, onOpenSettings }: Props) {
  return (
    <nav className="rail" aria-label="Primary">
      <div className="rail__logo" aria-hidden="true">
        H
      </div>
      <button
        type="button"
        className={"rail__btn" + (meetingsOpen ? " is-active" : "")}
        aria-label={meetingsOpen ? "Hide meetings" : "Show meetings"}
        aria-pressed={meetingsOpen}
        title="Meetings"
        onClick={onToggleMeetings}
      >
        <svg width="18" height="18" viewBox="0 0 18 18" fill="none" aria-hidden="true">
          <path
            d="M2.5 4.5h13M2.5 9h13M2.5 13.5h13"
            stroke="currentColor"
            strokeWidth="1.6"
            strokeLinecap="round"
          />
        </svg>
      </button>
      <button
        type="button"
        className="rail__btn rail__btn--bottom"
        aria-label="Settings"
        title="Settings"
        onClick={onOpenSettings}
      >
        <svg width="18" height="18" viewBox="0 0 18 18" fill="none" aria-hidden="true">
          <circle cx="9" cy="9" r="2.4" stroke="currentColor" strokeWidth="1.6" />
          <path
            d="M9 1.5v2M9 14.5v2M1.5 9h2M14.5 9h2M3.7 3.7l1.4 1.4M12.9 12.9l1.4 1.4M14.3 3.7l-1.4 1.4M5.1 12.9l-1.4 1.4"
            stroke="currentColor"
            strokeWidth="1.6"
            strokeLinecap="round"
          />
        </svg>
      </button>
    </nav>
  );
}
