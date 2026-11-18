// A filtering event tap, as dictation, hotkey and scroll tools install, for the macOS input tests.
//
// usage: xa11y-taps --location hid|session --events keys,flags [--delay-ms N] [--drop]
//
// Prints "ready" when the tap is on. On SIGTERM it prints "held=<events> enabled=<bool>
// reenabled=<count>" and exits, so a test can prove that the tap held the events.
import CoreGraphics
import Foundation

let locations: [String: CGEventTapLocation] = ["hid": .cghidEventTap, "session": .cgSessionEventTap]
let groups: [String: [CGEventType]] = ["keys": [.keyDown, .keyUp], "flags": [.flagsChanged]]

func fail(_ message: String) -> Never {
  FileHandle.standardError.write("\(message)\n".data(using: .utf8)!)
  exit(2)
}

var location = CGEventTapLocation.cghidEventTap
var mask: CGEventMask = 0
var delayUs: UInt32 = 0
var drop = false
var replace = false
var dropRelease = false
var args = CommandLine.arguments.dropFirst()
while let arg = args.popFirst() {
  switch arg {
  case "--location":
    guard let l = locations[args.popFirst() ?? ""] else { fail("--location takes hid or session") }
    location = l
  case "--delay-ms":
    guard let ms = UInt32(args.popFirst() ?? "") else { fail("--delay-ms takes a number") }
    delayUs = ms * 1000
  case "--drop": drop = true
  case "--replace": replace = true
  case "--drop-command-release": dropRelease = true
  case "--events":
    for name in (args.popFirst() ?? "").split(separator: ",") {
      guard let types = groups[String(name)] else { fail("--events takes keys,flags") }
      for type in types { mask |= 1 << type.rawValue }
    }
  default: fail("unknown argument \(arg)")
  }
}
if mask == 0 { fail("--events is required") }

var port: CFMachPort?
var held = 0
var reenabled = 0

let callback: CGEventTapCallBack = { _, type, event, _ in
  if type == .tapDisabledByTimeout || type == .tapDisabledByUserInput {
    reenabled += 1
    CGEvent.tapEnable(tap: port!, enable: true)
    return Unmanaged.passUnretained(event)
  }
  held += 1
  usleep(delayUs)
  if drop || (dropRelease && type == .flagsChanged && !event.flags.contains(.maskCommand)) {
    return nil
  }
  if replace {
    guard let copy = event.copy() else { fail("CGEvent copy failed") }
    copy.setIntegerValueField(.eventSourceUserData, value: 0)
    return Unmanaged.passRetained(copy)
  }
  return Unmanaged.passUnretained(event)
}

setvbuf(stdout, nil, _IOLBF, 0)
guard let tap = CGEvent.tapCreate(tap: location, place: .headInsertEventTap, options: .defaultTap,
                                  eventsOfInterest: mask, callback: callback, userInfo: nil) else {
  fail("CGEventTapCreate failed: grant Accessibility to this program")
}
port = tap
CFRunLoopAddSource(CFRunLoopGetCurrent(), CFMachPortCreateRunLoopSource(nil, tap, 0), .commonModes)
CGEvent.tapEnable(tap: tap, enable: true)
signal(SIGTERM, SIG_IGN)
let term = DispatchSource.makeSignalSource(signal: SIGTERM, queue: .main)
term.setEventHandler {
  print("held=\(held) enabled=\(CGEvent.tapIsEnabled(tap: tap)) reenabled=\(reenabled)")
  exit(0)
}
term.resume()
print("ready")
CFRunLoopRun()
