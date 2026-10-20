// Classic (non-module) script in <head>: resolve the theme before first paint
// so dark mode never flashes white. state.js takes over after boot.
(function () {
  var p = new URLSearchParams(location.search).get('theme');
  try { p = p || localStorage.getItem('dshx-theme'); } catch (e) { /* storage blocked */ }
  if (p !== 'light' && p !== 'dark') p = matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light';
  document.documentElement.dataset.theme = p;
})();
