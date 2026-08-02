import { Nav } from './components/Nav'
import { Hero } from './components/Hero'
import { Story } from './components/Story'
import { Docs } from './components/Docs'
import { Footer } from './components/Footer'
import { DownloadProvider } from './components/download'

export default function App() {
  return (
    <DownloadProvider>
      <Nav />
      <main>
        <Hero />
        <Story />
        <Docs />
      </main>
      <Footer />
    </DownloadProvider>
  )
}
