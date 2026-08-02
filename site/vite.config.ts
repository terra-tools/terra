import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'
import tailwindcss from '@tailwindcss/vite'

// `base` decides where asset URLs point.
//
//   dev      -> '/'        so `pnpm dev` serves at http://localhost:5173/
//   build    -> '/terra/'  GitHub project pages serve from /<repo>/
//   preview  -> '/terra/'  must match the build it is serving, so that
//                          `pnpm preview` reproduces Pages exactly
//   BASE_PATH overrides all three — set it to '/' when a custom domain
//   (CNAME) is configured, since that serves from the root.
//
// The Pages workflow passes BASE_PATH explicitly, so the deployed value is
// visible in the workflow rather than implied by this default.
export default defineConfig(({ command, isPreview }) => ({
  base:
    process.env.BASE_PATH ?? (command === 'serve' && !isPreview ? '/' : '/terra/'),
  plugins: [react(), tailwindcss()],
}))
