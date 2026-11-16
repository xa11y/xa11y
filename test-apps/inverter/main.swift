// A wheel inverter, as Scroll Reverser and UnnaturalScrollWheels are: a filtering event tap
// that flips the deltas of line (mouse wheel) scroll events and leaves continuous ones alone.
//
// Prints "ready" when the tap is on. On SIGTERM it prints "inverted=<events> enabled=<bool>"
// and exits, so a test can prove that the inverter ran.
import CoreGraphics
import Foundation

let axes: [CGEventField] = [
  .scrollWheelEventDeltaAxis1, .scrollWheelEventDeltaAxis2,
  .scrollWheelEventFixedPtDeltaAxis1, .scrollWheelEventFixedPtDeltaAxis2,
  .scrollWheelEventPointDeltaAxis1, .scrollWheelEventPointDeltaAxis2,
]

var port: CFMachPort?
var inverted = 0

let callback: CGEventTapCallBack = { _, type, event, _ in
  if type == .tapDisabledByTimeout || type == .tapDisabledByUserInput {
    CGEvent.tapEnable(tap: port!, enable: true)
  } else if event.getIntegerValueField(.scrollWheelEventIsContinuous) == 0 {
    for axis in axes {
      event.setIntegerValueField(axis, value: -event.getIntegerValueField(axis))
    }
    inverted += 1
  }
  return Unmanaged.passUnretained(event)
}

setvbuf(stdout, nil, _IOLBF, 0)
guard let tap = CGEvent.tapCreate(tap: .cgSessionEventTap, place: .headInsertEventTap, options: .defaultTap,
                                  eventsOfInterest: 1 << CGEventType.scrollWheel.rawValue,
                                  callback: callback, userInfo: nil) else {
  FileHandle.standardError.write("CGEventTapCreate failed: grant Accessibility to this program\n".data(using: .utf8)!)
  exit(2)
}
port = tap
CFRunLoopAddSource(CFRunLoopGetCurrent(), CFMachPortCreateRunLoopSource(nil, tap, 0), .commonModes)
CGEvent.tapEnable(tap: tap, enable: true)
signal(SIGTERM, SIG_IGN)
let term = DispatchSource.makeSignalSource(signal: SIGTERM, queue: .main)
term.setEventHandler {
  print("inverted=\(inverted) enabled=\(CGEvent.tapIsEnabled(tap: tap))")
  exit(0)
}
term.resume()
print("ready")
CFRunLoopRun()
