import Module from "node:module"
import {
  afterAll,
  beforeAll,
  beforeEach,
  describe,
  expect,
  it,
  vi,
} from "vitest"
import type { RemoteTransportConfig, Transport } from "./types"

const environment = vi.hoisted(() => ({ value: "web" as "web" | "tauri" }))
const instances = vi.hoisted(() => ({
  webDestroyed: 0,
  remoteDestroyed: 0,
}))

class FixtureTransport implements Transport {
  constructor(private readonly desktop: boolean) {}
  call<T>(_command: string): Promise<T> {
    void _command
    return Promise.reject(new Error("not used"))
  }
  subscribe<T>(
    _event: string,
    _handler: (payload: T) => void
  ): Promise<() => void> {
    void _event
    void _handler
    return Promise.resolve(() => {})
  }
  isDesktop(): boolean {
    return this.desktop
  }
  destroy(): void {}
}

class FixtureWebTransport extends FixtureTransport {
  constructor(_baseUrl: string) {
    void _baseUrl
    super(false)
  }
}

class FixtureTauriTransport extends FixtureTransport {
  constructor() {
    super(true)
  }
}

class FixtureRemoteTransport extends FixtureTransport {
  constructor(_config: RemoteTransportConfig) {
    void _config
    super(true)
  }
}

type ModuleLoader = (
  request: string,
  parent: NodeModule | null,
  isMain: boolean
) => unknown
const moduleWithLoader = Module as unknown as { _load: ModuleLoader }
const originalLoad = moduleWithLoader._load

beforeAll(() => {
  moduleWithLoader._load = (request, parent, isMain) => {
    if (request === "./web-transport") {
      return { WebTransport: FixtureWebTransport }
    }
    if (request === "./tauri-transport") {
      return { TauriTransport: FixtureTauriTransport }
    }
    if (request === "./remote-desktop-transport") {
      return { RemoteDesktopTransport: FixtureRemoteTransport }
    }
    return originalLoad(request, parent, isMain)
  }
})

afterAll(() => {
  moduleWithLoader._load = originalLoad
})

vi.mock("./detect", () => ({ detectEnvironment: () => environment.value }))

import {
  __resetTransportForTests,
  configureRemoteDesktopTransport,
  getActiveRemoteConnectionId,
  getShellTransport,
  getTransport,
  setBrowserServerMode,
} from "./index"

beforeEach(() => {
  __resetTransportForTests()
  environment.value = "web"
  instances.webDestroyed = 0
  instances.remoteDestroyed = 0
})

describe("browser transport mode gate", () => {
  it.each(["unknown", "team", "native"] as const)(
    "refuses browser shell transport in %s mode",
    (mode) => {
      setBrowserServerMode(mode)
      expect(() => getShellTransport()).toThrow(/not available/i)
      expect(() => getTransport()).toThrow(/not available/i)
    }
  )

  it("allows the browser shell only after explicit operator capability", () => {
    setBrowserServerMode("operator")
    expect(getShellTransport().isDesktop()).toBe(false)
  })

  it.each(["unknown", "team", "native"] as const)(
    "refuses browser remote transport creation in %s mode",
    (mode) => {
      setBrowserServerMode(mode)
      expect(() =>
        configureRemoteDesktopTransport({
          id: 6,
          name: "denied fixture",
          baseUrl: "https://remote.example",
          token: "invented-token",
          windowInstanceId: "fixture-window",
        })
      ).toThrow(/not available/i)
      expect(getActiveRemoteConnectionId()).toBeNull()
    }
  )

  it("preserves explicit browser operator remote transport behavior", () => {
    setBrowserServerMode("operator")
    configureRemoteDesktopTransport({
      id: 7,
      name: "operator remote",
      baseUrl: "https://remote.example",
      token: "invented-token",
      windowInstanceId: "fixture-window",
    })
    expect(getActiveRemoteConnectionId()).toBe(7)
    expect(getTransport()).toBeDefined()
  })

  it.each(["team", "native"] as const)(
    "destroys cached browser shell transport when mode becomes %s",
    (mode) => {
      setBrowserServerMode("operator")
      const transport = getShellTransport()
      transport.destroy = () => {
        instances.webDestroyed += 1
      }

      setBrowserServerMode(mode)
      expect(instances.webDestroyed).toBe(1)
      expect(() => getTransport()).toThrow(/not available/i)
    }
  )

  it.each(["team", "native"] as const)(
    "destroys and denies a cached remote transport on browser entry to %s mode",
    (mode) => {
      environment.value = "tauri"
      configureRemoteDesktopTransport({
        id: 7,
        name: "fixture",
        baseUrl: "https://remote.example",
        token: "invented-token",
        windowInstanceId: "fixture-window",
      })
      const cached = getTransport()
      cached.destroy = () => {
        instances.remoteDestroyed += 1
      }

      environment.value = "web"
      setBrowserServerMode(mode)
      expect(instances.remoteDestroyed).toBe(1)
      expect(getActiveRemoteConnectionId()).toBeNull()
      expect(() => getTransport()).toThrow(/not available/i)
    }
  )

  it("preserves native shell and remote desktop behavior", () => {
    environment.value = "tauri"
    setBrowserServerMode("unknown")
    expect(getShellTransport().isDesktop()).toBe(true)

    configureRemoteDesktopTransport({
      id: 9,
      name: "native remote",
      baseUrl: "https://remote.example",
      token: "invented-token",
      windowInstanceId: "fixture-window",
    })
    expect(getActiveRemoteConnectionId()).toBe(9)
    expect(getTransport()).toBeDefined()
  })
})
