import { createApp } from "vue";
import App from "./App.vue";
import { initLocale, t } from "./i18n";
import "./style.css";

const app = createApp(App);
app.config.globalProperties.$t = t;
void initLocale().finally(() => app.mount("#app"));
