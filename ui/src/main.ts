import { mount } from 'svelte'
import './app.css'
import App from './App.svelte'
import StandaloneMail from './lib/StandaloneMail.svelte'

// Same Vite bundle, two entry points: the main 3-pane app, or a
// single-message standalone reader (#104).  The route is selected
// via `?view=mail&account=…&folder=…&uid=…` in the query string,
// which the standalone-window helper sets when calling
// `WebviewWindow`.  Anything else falls through to the full app.
const params = new URLSearchParams(window.location.search)
const target = document.getElementById('app')!

let app
if (params.get('view') === 'mail') {
  const accountId = params.get('account') ?? ''
  const folder = params.get('folder') ?? 'INBOX'
  const uid = Number.parseInt(params.get('uid') ?? '0', 10)
  app = mount(StandaloneMail, {
    target,
    props: { accountId, folder, uid },
  })
} else {
  app = mount(App, { target })
}

export default app
