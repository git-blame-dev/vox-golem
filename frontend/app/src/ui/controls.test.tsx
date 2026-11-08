import { act } from 'react'
import { createRoot } from 'react-dom/client'
import { expect, it } from 'vitest'
import { SelectSetting, SwitchField } from './controls'

it('preserves caller descriptions alongside shared control supporting text', () => {
  const container = document.createElement('div')
  const root = createRoot(container)
  try {
    act(() => root.render(<>
      <p id="synthetic-status">Synthetic status</p>
      <SelectSetting label="Synthetic selection" description="Selection help" aria-describedby="synthetic-status"><option>Automatic</option></SelectSetting>
      <SwitchField label="Synthetic switch" description="Switch help" aria-describedby="synthetic-status" />
    </>))
    for (const control of container.querySelectorAll('select,input')) {
      const ids = control.getAttribute('aria-describedby')!.split(' ')
      expect(ids).toHaveLength(2)
      expect(ids[0]).toBe('synthetic-status')
      expect(container.querySelector(`[id="${ids[1]}"]`)?.textContent).toMatch(/help$/)
    }
  } finally {
    act(() => root.unmount())
  }
})
