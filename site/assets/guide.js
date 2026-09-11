function revealLinkedAnswer() {
  let id;
  try { id = decodeURIComponent(location.hash.slice(1)); } catch { return; }
  const target = document.getElementById(id);
  if (target?.tagName === 'DETAILS') target.open = true;
}
window.addEventListener('hashchange', revealLinkedAnswer);
revealLinkedAnswer();
