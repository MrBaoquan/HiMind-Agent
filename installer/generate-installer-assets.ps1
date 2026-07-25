param(
    [string]$OutputDirectory = (Join-Path $PSScriptRoot "generated")
)

$ErrorActionPreference = "Stop"
Add-Type -AssemblyName System.Drawing

$outputPath = [System.IO.Path]::GetFullPath($OutputDirectory)
New-Item -ItemType Directory -Force -Path $outputPath | Out-Null
$brandIconPath = Join-Path $PSScriptRoot "..\icons\himind-app.png"
$script:BrandIcon = [System.Drawing.Image]::FromFile([System.IO.Path]::GetFullPath($brandIconPath))

function Draw-AgentMark {
    param(
        [System.Drawing.Graphics]$Graphics,
        [float]$X,
        [float]$Y,
        [float]$Size
    )

    $Graphics.DrawImage($script:BrandIcon, $X, $Y, $Size, $Size)
}

function New-InstallerBitmap {
    param(
        [int]$Width,
        [int]$Height,
        [string]$Path,
        [scriptblock]$Draw
    )

    $bitmap = [System.Drawing.Bitmap]::new($Width, $Height, [System.Drawing.Imaging.PixelFormat]::Format24bppRgb)
    $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
    try {
        $graphics.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
        $graphics.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
        $graphics.TextRenderingHint = [System.Drawing.Text.TextRenderingHint]::ClearTypeGridFit
        & $Draw $graphics
        $bitmap.Save($Path, [System.Drawing.Imaging.ImageFormat]::Bmp)
    } finally {
        $graphics.Dispose()
        $bitmap.Dispose()
    }
}

$welcomePath = Join-Path $outputPath "installer-welcome.bmp"
New-InstallerBitmap -Width 164 -Height 314 -Path $welcomePath -Draw {
    param($graphics)

    $canvas = [System.Drawing.SolidBrush]::new([System.Drawing.ColorTranslator]::FromHtml("#F5F8FF"))
    $rail = [System.Drawing.SolidBrush]::new([System.Drawing.ColorTranslator]::FromHtml("#102A4C"))
    $primary = [System.Drawing.Pen]::new([System.Drawing.ColorTranslator]::FromHtml("#2563EB"), 2)
    $cyan = [System.Drawing.Pen]::new([System.Drawing.ColorTranslator]::FromHtml("#0891B2"), 2)
    $grid = [System.Drawing.Pen]::new([System.Drawing.ColorTranslator]::FromHtml("#D9E4F5"), 1)
    $nodeFill = [System.Drawing.SolidBrush]::new([System.Drawing.Color]::White)
    $textBrush = [System.Drawing.SolidBrush]::new([System.Drawing.ColorTranslator]::FromHtml("#102033"))
    $subtleBrush = [System.Drawing.SolidBrush]::new([System.Drawing.ColorTranslator]::FromHtml("#52657F"))
    $brandFont = [System.Drawing.Font]::new("Segoe UI", 14, [System.Drawing.FontStyle]::Bold, [System.Drawing.GraphicsUnit]::Pixel)
    $agentFont = [System.Drawing.Font]::new("Segoe UI", 8, [System.Drawing.FontStyle]::Regular, [System.Drawing.GraphicsUnit]::Pixel)

    try {
        $graphics.FillRectangle($canvas, 0, 0, 164, 314)
        $graphics.FillRectangle($rail, 0, 0, 6, 314)
        for ($x = 22; $x -lt 164; $x += 28) {
            $graphics.DrawLine($grid, $x, 142, $x, 286)
        }
        for ($y = 146; $y -lt 300; $y += 28) {
            $graphics.DrawLine($grid, 16, $y, 150, $y)
        }

        Draw-AgentMark -Graphics $graphics -X 22 -Y 24 -Size 42
        $graphics.DrawString("HiMind", $brandFont, $textBrush, 22, 78)
        $graphics.DrawString("AGENT", $agentFont, $subtleBrush, 23, 100)

        $graphics.DrawLine($primary, 30, 238, 66, 204)
        $graphics.DrawLine($primary, 66, 204, 104, 226)
        $graphics.DrawLine($cyan, 104, 226, 134, 184)
        foreach ($point in @(@(30, 238), @(66, 204), @(104, 226), @(134, 184))) {
            $graphics.FillEllipse($nodeFill, $point[0] - 5, $point[1] - 5, 10, 10)
            $graphics.DrawEllipse($primary, $point[0] - 5, $point[1] - 5, 10, 10)
        }
    } finally {
        $canvas.Dispose()
        $rail.Dispose()
        $primary.Dispose()
        $cyan.Dispose()
        $grid.Dispose()
        $nodeFill.Dispose()
        $textBrush.Dispose()
        $subtleBrush.Dispose()
        $brandFont.Dispose()
        $agentFont.Dispose()
    }
}

$headerPath = Join-Path $outputPath "installer-header.bmp"
New-InstallerBitmap -Width 150 -Height 57 -Path $headerPath -Draw {
    param($graphics)

    $canvas = [System.Drawing.SolidBrush]::new([System.Drawing.Color]::White)
    $line = [System.Drawing.Pen]::new([System.Drawing.ColorTranslator]::FromHtml("#D9E4F5"), 1)
    $primary = [System.Drawing.Pen]::new([System.Drawing.ColorTranslator]::FromHtml("#2563EB"), 2)
    $node = [System.Drawing.SolidBrush]::new([System.Drawing.ColorTranslator]::FromHtml("#0891B2"))
    try {
        $graphics.FillRectangle($canvas, 0, 0, 150, 57)
        $graphics.DrawLine($line, 0, 56, 150, 56)
        $graphics.DrawLine($primary, 88, 35, 112, 20)
        $graphics.DrawLine($primary, 112, 20, 130, 32)
        $graphics.FillEllipse($node, 84, 31, 8, 8)
        $graphics.FillEllipse($node, 108, 16, 8, 8)
        Draw-AgentMark -Graphics $graphics -X 116 -Y 18 -Size 28
    } finally {
        $canvas.Dispose()
        $line.Dispose()
        $primary.Dispose()
        $node.Dispose()
    }
}

function New-AgentIconPng {
    param([int]$Size)

    $bitmap = [System.Drawing.Bitmap]::new($Size, $Size, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
    try {
        $graphics.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
        $graphics.Clear([System.Drawing.Color]::Transparent)
        Draw-AgentMark -Graphics $graphics -X 0 -Y 0 -Size $Size
        $stream = [System.IO.MemoryStream]::new()
        $bitmap.Save($stream, [System.Drawing.Imaging.ImageFormat]::Png)
        return ,$stream.ToArray()
    } finally {
        $graphics.Dispose()
        $bitmap.Dispose()
    }
}

$iconSizes = @(16, 24, 32, 48, 64, 256)
$iconImages = @($iconSizes | ForEach-Object { New-AgentIconPng -Size $_ })
$iconPath = Join-Path $outputPath "himind-agent.ico"
$iconStream = [System.IO.File]::Create($iconPath)
$writer = [System.IO.BinaryWriter]::new($iconStream)
try {
    $writer.Write([uint16]0)
    $writer.Write([uint16]1)
    $writer.Write([uint16]$iconImages.Count)

    $offset = 6 + (16 * $iconImages.Count)
    for ($index = 0; $index -lt $iconImages.Count; $index++) {
        $size = $iconSizes[$index]
        $writer.Write([byte]$(if ($size -eq 256) { 0 } else { $size }))
        $writer.Write([byte]$(if ($size -eq 256) { 0 } else { $size }))
        $writer.Write([byte]0)
        $writer.Write([byte]0)
        $writer.Write([uint16]1)
        $writer.Write([uint16]32)
        $writer.Write([uint32]$iconImages[$index].Length)
        $writer.Write([uint32]$offset)
        $offset += $iconImages[$index].Length
    }

    foreach ($image in $iconImages) {
        $writer.Write($image)
    }
} finally {
    $writer.Dispose()
    $iconStream.Dispose()
    $script:BrandIcon.Dispose()
}

Write-Host "Installer assets generated: $outputPath"
