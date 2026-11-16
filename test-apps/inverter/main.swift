// A wheel inverter, as Scroll Reverser and UnnaturalScrollWheels are: a filtering event tap
// that flips the deltas of line (mouse wheel) scroll events and leaves continuous ones alone.
//
// Prints "ready" when the tap is on. On SIGTERM it prints "inverted=<events> enabled=<bool>"
// and exits, so a test can prove that the inverter ran.
import CoreGraphics
import Foundation

let fixedPoint: Set<CGEventField> = [.scrollWheelEventFixedPtDeltaAxis1, .scrollWheelEventFixedPtDeltaAxis2]
let axes: [CGEventField] = [
  .scrollWheelEventDeltaAxis1, .scrollWheelEventDeltaAxis2,
  .scrollWheelEventFixedPtDeltaAxis1, .scrollWheelEventFixedPtDeltaAxis2,
  .scrollWheelEventPointDeltaAxis1, .scrollWheelEventPointDeltaAxis2,
]

func read(_ event: CGEvent, _ axis: CGEventField) -> Double {
  fixedPoint.contains(axis) ? event.getDoubleValueField(axis) : Double(event.getIntegerValueField(axis))
}

func write(_ event: CGEvent, _ axis: CGEventField, _ value: Double) {
  if fixedPoint.contains(axis) {
    event.setDoubleValueField(axis, value: value)
  } else {
    event.setIntegerValueField(axis, value: Int64(value))
  }
}

var port: CFMachPort?
var inverted = 0

let callback: CGEventTapCallBack = { _, type, event, _ in
  if type == .tapDisabledByTimeout || type == .tapDisabledByUserInput {
    CGEvent.tapEnable(tap: port!, enable: true)
    return Unmanaged.passUnretained(event)
  }
  let continuous = event.getIntegerValueField(.scrollWheelEventIsContinuous) != 0
  // Read every axis before the first write: a write to a DeltaAxis field makes CoreGraphics recompute the others.
  let before = axes.map { read(event, $0) }
  if !continuous {
    for (axis, value) in zip(axes, before) {
      write(event, axis, -value)
    }
    inverted += 1
  }
  let after = axes.map { read(event, $0) }
  FileHandle.standardError.write("continuous=\(continuous) at=\(event.location) before=\(before) after=\(after)\n".data(using: .utf8)!)
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
