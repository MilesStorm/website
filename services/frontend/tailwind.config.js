/** @type {import('tailwindcss').Config} */
module.exports = {
  mode: "all",
  content: ["./src/**/*.{rs,html,css}", "./dist/**/*.html"],
  darkMode: ["class", '[data-theme="dark"]'],
  daisyui: {
    themes: ['light', 'dark', 'synthwave', 'dracula', 'retro', 'dim', 'corporate'],
    style: true,
    base: true,
    utils: true,
    logs: true,
    rtl: false,
    prefix: '',
  },
  theme: {
    extend: {
      keyframes: {
        'gradient': {
          to: { 'background-position': '200% center' }
        }
      },
      animation: {
        'gradient': 'gradient 8s linear infinite'
      }
    }
  },
  plugins: [require("@tailwindcss/typography"), require("daisyui")]
};
