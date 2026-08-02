import { Nav } from './components/Nav'
import { Hero } from './components/Hero'
import { Story } from './components/Story'
import { Install } from './components/Install'
import { Docs } from './components/Docs'
import { Footer } from './components/Footer'

export default function App() {
  return (
    <>
      <Nav />
      <main>
        <Hero />
        <Story />
        <Install />
        <Docs />
      </main>
      <Footer />
    </>
  )
}
