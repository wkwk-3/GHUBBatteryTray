# SVG の path (16x16) を WPF で描いて、size x size のアルファ値 (1 バイト/画素) にする
param([string]$svg, [string]$out, [int]$size = 64)
Add-Type -AssemblyName PresentationCore, WindowsBase
$text = Get-Content $svg -Raw
$d = [regex]::Match($text, 'd="([^"]+)"').Groups[1].Value
$geo = [System.Windows.Media.Geometry]::Parse($d)
$dv = New-Object System.Windows.Media.DrawingVisual
$dc = $dv.RenderOpen()
$scale = $size / 16.0
$dc.PushTransform((New-Object System.Windows.Media.ScaleTransform $scale, $scale))
$dc.DrawGeometry([System.Windows.Media.Brushes]::White, $null, $geo)
$dc.Close()
$bmp = New-Object System.Windows.Media.Imaging.RenderTargetBitmap $size, $size, 96, 96, ([System.Windows.Media.PixelFormats]::Pbgra32)
$bmp.Render($dv)
$px = New-Object byte[] ($size * $size * 4)
$bmp.CopyPixels($px, $size * 4, 0)
$alpha = New-Object byte[] ($size * $size)
for ($i = 0; $i -lt $alpha.Length; $i++) { $alpha[$i] = $px[$i * 4 + 3] }
[IO.File]::WriteAllBytes($out, $alpha)
"wrote $out ($size x $size)"
