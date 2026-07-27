interface Props {
  onOpenSettings: () => void;
  // Return to the home dashboard (clears the selected meeting).
  onGoHome: () => void;
  // Open the "new recording" popover; recordActive highlights the button while it's open.
  onOpenRecord: () => void;
  recordActive: boolean;
  // Open the Library (the full meetings browser: folders + meeting list); meetingsActive highlights it.
  onOpenMeetings: () => void;
  meetingsActive: boolean;
  // Open the global transcript search popover; searchActive highlights the item while it's open.
  onOpenSearch: () => void;
  searchActive: boolean;
}

// The slim, always-present left icon rail. The logo returns to the home dashboard; the red button
// opens the new-recording popover; the list icon opens the Library (meetings + folders); the
// magnifier opens global search; the gear opens Settings. Branding pins the top, the gear the bottom.
export function NavRail({
  onOpenSettings,
  onGoHome,
  onOpenRecord,
  recordActive,
  onOpenMeetings,
  meetingsActive,
  onOpenSearch,
  searchActive,
}: Props) {
  return (
    <nav className="rail" aria-label="Primary">
      {/* Inlined so it renders in the packaged app too — the core serves only `/` and `/assets/*`,
          not root public files like the icon SVG. */}
      <button type="button" className="rail__logo" aria-label="Home" title="Home" onClick={onGoHome}>
        <svg viewBox="0 0 100 100" width={30} height={30} aria-hidden="true">
          <rect width="100" height="100" rx="20" fill="#4b37c9" />
          <polyline
            points="20,52 32,30 44,64 56,26 68,60 80,44"
            fill="none"
            stroke="#ffffff"
            strokeWidth={11}
            strokeLinecap="round"
            strokeLinejoin="round"
          />
        </svg>
      </button>
      <button
        type="button"
        className={"rail__record" + (recordActive ? " is-active" : "")}
        aria-label="New recording"
        aria-expanded={recordActive}
        title="New recording"
        onClick={onOpenRecord}
      >
        <svg width="16" height="16" viewBox="0 0 24 24" aria-hidden="true">
          <circle cx="12" cy="12" r="6" fill="currentColor" />
        </svg>
      </button>
      <button
        type="button"
        className={"rail__btn" + (meetingsActive ? " is-active" : "")}
        aria-label="Meetings"
        aria-current={meetingsActive ? "page" : undefined}
        title="Meetings"
        onClick={onOpenMeetings}
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
        className={"rail__btn" + (searchActive ? " is-active" : "")}
        aria-label="Search transcripts"
        aria-expanded={searchActive}
        title="Search transcripts"
        onClick={onOpenSearch}
      >
        <svg
          width="18"
          height="18"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          strokeWidth="1.8"
          strokeLinecap="round"
          strokeLinejoin="round"
          aria-hidden="true"
        >
          <circle cx="11" cy="11" r="7" />
          <path d="M20 20l-3.9-3.9" />
        </svg>
      </button>
      <button
        type="button"
        className="rail__btn rail__btn--bottom"
        aria-label="Settings"
        title="Settings"
        onClick={onOpenSettings}
      >
        <svg
          width="19"
          height="19"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          strokeWidth="1.6"
          strokeLinecap="round"
          strokeLinejoin="round"
          aria-hidden="true"
        >
          <path d="M10.34 3.94c.09-.54.56-.94 1.11-.94h1.1c.55 0 1.02.4 1.11.94l.16.98c.05.3.26.55.54.68.1.05.2.1.3.16.28.16.62.17.9.03l.9-.42c.5-.24 1.1-.05 1.37.43l.55.95c.28.48.15 1.09-.29 1.42l-.79.6c-.25.18-.38.48-.35.79a3.6 3.6 0 0 1 0 .36c-.03.3.1.6.35.79l.79.6c.44.33.57.94.29 1.42l-.55.95c-.28.48-.88.67-1.37.43l-.9-.42c-.28-.14-.62-.13-.9.03-.1.06-.2.11-.3.16-.28.13-.49.38-.54.68l-.16.98c-.09.54-.56.94-1.11.94h-1.1c-.55 0-1.02-.4-1.11-.94l-.16-.98c-.05-.3-.26-.55-.54-.68a3.5 3.5 0 0 1-.3-.16c-.28-.16-.62-.17-.9-.03l-.9.42c-.5.24-1.1.05-1.37-.43l-.55-.95c-.28-.48-.15-1.09.29-1.42l.79-.6c.25-.19.38-.49.35-.79a3.6 3.6 0 0 1 0-.36c.03-.31-.1-.61-.35-.79l-.79-.6c-.44-.33-.57-.94-.29-1.42l.55-.95c.28-.48.88-.67 1.37-.43l.9.42c.28.14.62.13.9-.03.1-.05.2-.11.3-.16.28-.13.49-.38.54-.68l.16-.98Z" />
          <circle cx="12" cy="12" r="2.6" />
        </svg>
      </button>
    </nav>
  );
}
