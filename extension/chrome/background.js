// Clicking the toolbar button opens the side panel.
chrome.sidePanel.setPanelBehavior({ openPanelOnActionClick: true }).catch(console.error);

// The side panel mirrors the latest roll into chrome.storage.session. Let this
// extension's future content scripts (e.g. one that types the roll into a
// website's text box) read it; web pages themselves still can't.
chrome.storage.session
  .setAccessLevel({ accessLevel: "TRUSTED_AND_UNTRUSTED_CONTEXTS" })
  .catch(console.error);
