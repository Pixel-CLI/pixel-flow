// SPDX-FileCopyrightText: The Pixel contributors
// SPDX-License-Identifier: MIT

// Recognise the text lines of one PNG with macOS Vision and print them as a
// JSON array of {text, x, y, w, h} in image pixels, origin top-left.
import Foundation
import ImageIO
import Vision

let url = URL(fileURLWithPath: CommandLine.arguments[1])
guard let source = CGImageSourceCreateWithURL(url as CFURL, nil),
    let image = CGImageSourceCreateImageAtIndex(source, 0, nil)
else {
    FileHandle.standardError.write("cannot read \(url.path)\n".data(using: .utf8)!)
    exit(2)
}
let request = VNRecognizeTextRequest()
request.recognitionLevel = .accurate
request.usesLanguageCorrection = false
try VNImageRequestHandler(cgImage: image, options: [:]).perform([request])
let width = Double(image.width)
let height = Double(image.height)
var lines: [[String: Any]] = []
for observation in request.results ?? [] {
    guard let best = observation.topCandidates(1).first else { continue }
    let box = observation.boundingBox
    lines.append([
        "text": best.string,
        "x": box.minX * width,
        "y": (1 - box.maxY) * height,
        "w": box.width * width,
        "h": box.height * height,
    ])
}
let json = try JSONSerialization.data(withJSONObject: lines)
print(String(data: json, encoding: .utf8)!)
