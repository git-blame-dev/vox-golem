import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import type { CSSProperties } from 'react'
import type { AssistantOption, InstantChoice } from '../lib/assistantSettings'
import { MODEL_CATALOG } from '../lib/modelCatalog'
import { ControlIcon } from './ControlIcon'

interface InstantModelSelectProps {
  readonly id: string
  readonly label: string
  readonly value: InstantChoice
  readonly options: readonly AssistantOption<InstantChoice>[]
  readonly disabled: boolean
  readonly canRetry: (value: InstantChoice) => boolean
  readonly onChange: (value: InstantChoice) => void
  readonly variant?: 'default' | 'pill' | 'icon'
  readonly fastMode?: boolean
  readonly onFastModeChange?: (enabled: boolean) => void
}

const catalogMap = new Map(MODEL_CATALOG.map((m) => [m.id, m]))

const effortShortLabels: Record<string, string> = {
  none: 'None',
  low: 'Low',
  medium: 'Medium',
  high: 'High',
  xhigh: 'X-High',
  max: 'Max',
}

const EXIT_ANIMATION_MS = 160

export function InstantModelSelect({ id, label, value, options, disabled, canRetry, onChange, variant = 'default', fastMode, onFastModeChange }: InstantModelSelectProps) {
  const [popupPhase, setPopupPhase] = useState<'closed' | 'open' | 'closing'>('closed')
  const closeTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null)
  const triggerRef = useRef<HTMLButtonElement>(null)
  const popupRef = useRef<HTMLDivElement>(null)
  const keyboardEntryRef = useRef(false)

  const clearCloseTimer = useCallback(() => {
    if (closeTimerRef.current !== null) {
      clearTimeout(closeTimerRef.current)
      closeTimerRef.current = null
    }
  }, [])

  const closePopup = useCallback((reason: 'restore' | 'outside' = 'outside') => {
    const trigger = triggerRef.current
    const active = document.activeElement
    if (reason === 'restore' && trigger?.isConnected && !trigger.disabled
      && (active === trigger || (active !== null && popupRef.current?.contains(active)))) {
      trigger.focus()
    }
    clearCloseTimer()
    setPopupPhase((prev) => {
      if (prev === 'closed') return 'closed'
      return 'closing'
    })
    closeTimerRef.current = setTimeout(() => {
      setPopupPhase('closed')
      closeTimerRef.current = null
    }, EXIT_ANIMATION_MS)
  }, [clearCloseTimer])

  const openPopup = useCallback((keyboard = false) => {
    clearCloseTimer()
    keyboardEntryRef.current = keyboard
    setPopupPhase('open')
  }, [clearCloseTimer])

  useEffect(() => () => clearCloseTimer(), [clearCloseTimer])

  useEffect(() => {
    if (popupPhase !== 'open') return
    const handleKeyDown = (event: KeyboardEvent) => {
      const target = event.target
      const owned = target instanceof Node && (triggerRef.current?.contains(target) || popupRef.current?.contains(target))
      if (event.key === 'Escape' && owned) {
        event.preventDefault()
        event.stopPropagation()
        closePopup('restore')
      }
    }
    const handlePointerDown = (event: PointerEvent) => {
      const target = event.target
      if (target instanceof Node && !triggerRef.current?.contains(target) && !popupRef.current?.contains(target)) closePopup('outside')
    }
    const handleFocusIn = (event: FocusEvent) => {
      const target = event.target
      if (target instanceof Node && !triggerRef.current?.contains(target) && !popupRef.current?.contains(target)) closePopup('outside')
    }
    window.addEventListener('keydown', handleKeyDown)
    window.addEventListener('pointerdown', handlePointerDown)
    window.addEventListener('focusin', handleFocusIn)
    return () => {
      window.removeEventListener('keydown', handleKeyDown)
      window.removeEventListener('pointerdown', handlePointerDown)
      window.removeEventListener('focusin', handleFocusIn)
    }
  }, [popupPhase, closePopup])

  const selectedCatalog = catalogMap.get(value)
  const currentFamilyId = selectedCatalog?.familyId ?? value
  const currentEffort = selectedCatalog?.effort ?? 'none'
  const supportsFast = selectedCatalog?.supportsFast === true

  const { families, selectedOption, selectedLabel, pillDisplayLabel, familyOptions, familyEfforts } = useMemo(() => {
    const famMap = new Map<string, { familyId: string; familyLabel: string; models: AssistantOption<InstantChoice>[] }>()
    for (const opt of options) {
      const cat = catalogMap.get(opt.value)
      const famId = cat?.familyId ?? opt.value
      const famLabel = cat?.shortFamilyLabel ?? opt.label
      let fam = famMap.get(famId)
      if (!fam) {
        fam = { familyId: famId, familyLabel: famLabel, models: [] }
        famMap.set(famId, fam)
      }
      fam.models.push(opt)
    }
    const famList = Array.from(famMap.values())
    const selFamily = famList.find((f) => f.familyId === currentFamilyId) ?? famList[0]
    const curFamilyModels = selFamily?.models ?? options
    const selOption = options.find((opt) => opt.value === value)
    const selLabel = selOption?.label ?? (options.length === 0 ? 'Models unavailable' : 'Selected model unavailable')

    const efforts = curFamilyModels.map((opt) => {
      const cat = catalogMap.get(opt.value)
      return { effort: cat?.effort ?? 'none', option: opt }
    })
    const multiEfforts = efforts.length > 1 || (efforts.length === 1 && efforts[0]?.effort !== 'none')

    let pillLabel = selLabel
    if (selOption && selectedCatalog && !selOption.label.startsWith('Synthetic')) {
      const effortSuffix = currentEffort !== 'none'
        ? ` ${effortShortLabels[currentEffort] ?? currentEffort}`
        : multiEfforts
          ? ' None'
          : ''
      const fastSuffix = supportsFast && fastMode ? ' · Fast' : ''
      const previewSuffix = selOption.label.endsWith(' (browser preview)') && !selectedCatalog.shortFamilyLabel.endsWith(' (browser preview)')
        ? ' (browser preview)'
        : ''
      pillLabel = `${selectedCatalog.shortFamilyLabel}${effortSuffix}${fastSuffix}${previewSuffix}`
    }

    const famOpts = famList.map((fam) => {
      const isCurrentFamily = fam.familyId === currentFamilyId && selOption !== undefined
      const repOption = isCurrentFamily
        ? selOption
        : fam.models.find((opt) => (catalogMap.get(opt.value)?.effort ?? 'none') === currentEffort && (opt.available || canRetry(opt.value)))
          ?? fam.models.find((opt) => opt.available || canRetry(opt.value))
          ?? fam.models.find((opt) => (catalogMap.get(opt.value)?.effort ?? 'none') === currentEffort)
          ?? fam.models[0]!
      const available = fam.models.some((opt) => opt.available || canRetry(opt.value))
      const baseLabel = repOption.label.startsWith('Synthetic')
        ? repOption.label
        : repOption.label.endsWith(' (browser preview)') && !fam.familyLabel.endsWith(' (browser preview)')
          ? `${fam.familyLabel} (browser preview)`
          : fam.familyLabel
      return {
        familyId: fam.familyId,
        isCurrentFamily,
        modelCount: fam.models.length,
        repOption,
        available,
        displayLabel: available ? baseLabel : `${baseLabel} — ${repOption.reason ?? 'Unavailable'}`,
      }
    })

    return {
      families: famList,
      selectedOption: selOption,
      selectedLabel: selLabel,
      pillDisplayLabel: pillLabel,
      familyOptions: famOpts,
      familyEfforts: efforts,
    }
  }, [options, value, currentFamilyId, currentEffort, selectedCatalog, canRetry, supportsFast, fastMode])

  const effectivePhase = options.length === 0 ? 'closed' : popupPhase
  const isPopupVisible = effectivePhase !== 'closed'
  const isPopupOpen = effectivePhase === 'open'

  useEffect(() => {
    if (!isPopupOpen || !keyboardEntryRef.current) return
    keyboardEntryRef.current = false
    if (disabled || document.activeElement !== triggerRef.current) return
    const popup = popupRef.current
    const option = popup?.querySelector<HTMLButtonElement>('[role="option"][aria-selected="true"]:not(:disabled)')
      ?? popup?.querySelector<HTMLButtonElement>('[role="option"]:not(:disabled)')
    option?.focus()
  }, [isPopupOpen, disabled])

  const renderNativeSelect = (extraClassName?: string, nonInteractive = false) => (
    <select
      id={id}
      aria-label={label || 'Model'}
      title={selectedLabel}
      value={selectedOption ? value : ''}
      disabled={disabled || options.length === 0}
      className={extraClassName}
      tabIndex={nonInteractive ? -1 : undefined}
      style={nonInteractive ? { pointerEvents: 'none' } : undefined}
      onChange={(event) => {
        if (disabled) return
        const val = event.target.value
        const option = options.find((candidate) => candidate.value === val)
        if (option) {
          if (option.available || canRetry(option.value)) onChange(option.value)
          return
        }
        const fam = families.find((f) => f.familyId === val)
        if (fam && fam.models.length > 0) {
          const matchingEffort = fam.models.find((opt) => {
            const cat = catalogMap.get(opt.value)
            return (cat?.effort ?? 'none') === currentEffort && (opt.available || canRetry(opt.value))
          }) ?? fam.models.find((opt) => opt.available || canRetry(opt.value))
          if (matchingEffort) onChange(matchingEffort.value)
        }
      }}
    >
      {!selectedOption ? <option value="" disabled>{options.length === 0 ? 'Models unavailable' : 'Selected model unavailable'}</option> : null}
      {options.map((option) => (
        <option key={option.value} value={option.value} disabled={!option.available && !canRetry(option.value)} title={option.reason}>
          {option.available || canRetry(option.value) ? option.label : `${option.label} — ${option.reason ?? 'Unavailable'}`}
        </option>
      ))}
    </select>
  )

  if (variant === 'default') {
    return renderNativeSelect()
  }

  if (variant === 'pill') {
    return (
      <div className="composer__model-selectors">
        <div className="composer__model-pill native-icon-select" title={selectedLabel}>
          <button
            ref={triggerRef}
            type="button"
            className="composer__model-pill-trigger"
            aria-haspopup="dialog"
            aria-expanded={isPopupOpen}
            disabled={disabled || options.length === 0}
            onKeyDown={(event) => {
              if (['ArrowDown', 'Enter', ' '].includes(event.key) && !disabled && options.length > 0) {
                event.preventDefault()
                openPopup(true)
              }
            }}
            onClick={(event) => {
              if (disabled || options.length === 0) return
              if (isPopupOpen) closePopup()
              else openPopup(event.detail === 0 && document.activeElement === event.currentTarget)
            }}
          >
            <span className="composer__model-pill-label">{pillDisplayLabel}</span>
            <svg className="composer__model-pill-chevron" data-open={isPopupOpen ? 'true' : undefined} viewBox="0 0 16 16" aria-hidden="true" focusable="false">
              <path d="M4 6l4 4 4-4" />
            </svg>
          </button>
          {renderNativeSelect('sr-only composer__model-native-fallback', true)}
        </div>
        {isPopupVisible ? (
          <>
            <div
              className="composer__model-backdrop"
              data-state={effectivePhase}
              aria-hidden="true"
              onClick={() => closePopup()}
            />
            <div
              ref={popupRef}
              className="composer__model-popup"
              inert={!isPopupOpen}
              data-state={effectivePhase}
              role="dialog"
              aria-label="Select model and reasoning effort"
            >
              <div className="composer__model-popup-list" role="listbox" aria-label="Models">
                {familyOptions.map(({ familyId, isCurrentFamily, modelCount, repOption, available, displayLabel }, index) => (
                  <button
                    key={familyId}
                    type="button"
                    role="option"
                    aria-selected={isCurrentFamily}
                    className="composer__model-popup-item"
                    data-selected={isCurrentFamily ? 'true' : undefined}
                    style={{ '--item-index': index } as CSSProperties}
                    disabled={!isPopupOpen || disabled || !available}
                    title={repOption.reason}
                    onClick={() => {
                      if (!isPopupOpen || disabled || !available) return
                      if (repOption.value !== value) {
                        onChange(repOption.value)
                      }
                      if (isCurrentFamily || modelCount <= 1) {
                        closePopup('restore')
                      }
                    }}
                  >
                    <span className="composer__model-popup-item-name">{displayLabel}</span>
                    {isCurrentFamily ? (
                      <span className="composer__model-popup-item-check" aria-hidden="true">
                        <ControlIcon kind="check" />
                      </span>
                    ) : null}
                  </button>
                ))}
              </div>
              <div className="composer__model-popup-efforts" role="group" aria-label="Reasoning effort">
                <div className="composer__model-popup-effort-row">
                  {familyEfforts.map(({ effort, option }) => {
                    const isCurrentEffort = option.value === value || (familyEfforts.length === 1 && effort === currentEffort)
                    const effortAvailable = option.available || canRetry(option.value)
                    const chipLabel = familyEfforts.length === 1 && effort === 'none'
                      ? 'Standard'
                      : (effortShortLabels[effort] ?? effort)
                    return (
                      <button
                        key={option.value}
                        type="button"
                        className="composer__model-popup-effort-chip"
                        data-active={isCurrentEffort ? 'true' : undefined}
                        aria-pressed={isCurrentEffort}
                        disabled={!isPopupOpen || disabled || !effortAvailable}
                        title={option.reason}
                        onClick={() => {
                          if (!isPopupOpen || disabled || !effortAvailable) return
                          if (option.value !== value) {
                            onChange(option.value)
                          }
                          closePopup('restore')
                        }}
                      >
                        {chipLabel}
                      </button>
                    )
                  })}
                </div>
                {supportsFast && onFastModeChange !== undefined ? (
                  <div className="composer__model-popup-fast-row" role="group" aria-label="Fast mode">
                    <span className="composer__model-popup-fast-label">Fast</span>
                    <div className="composer__model-popup-fast-chips">
                      {([
                        { label: 'Off', nextFast: false },
                        { label: 'On', nextFast: true },
                      ] as const).map(({ label: optLabel, nextFast }) => {
                        const isActive = Boolean(fastMode) === nextFast
                        const isDisabled = !isPopupOpen || disabled || (!selectedOption?.available && !canRetry(value))
                        return (
                          <button
                            key={optLabel}
                            type="button"
                            className="composer__model-popup-effort-chip composer__model-popup-fast-chip"
                            data-active={isActive ? 'true' : undefined}
                            aria-pressed={isActive}
                            aria-label={`Fast ${optLabel}`}
                            disabled={isDisabled}
                            onClick={() => {
                              if (isDisabled) return
                              if (Boolean(fastMode) !== nextFast) {
                                onFastModeChange(nextFast)
                              }
                              closePopup('restore')
                            }}
                          >
                            {optLabel}
                          </button>
                        )
                      })}
                    </div>
                  </div>
                ) : null}
              </div>
            </div>
          </>
        ) : null}
      </div>
    )
  }

  return (
    <span className="native-icon-select shell__icon-control" title={selectedLabel}>
      <ControlIcon kind="model" />
      {renderNativeSelect()}
    </span>
  )
}
