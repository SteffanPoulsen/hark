# Serves the files in -Root on http://localhost:<Port>/, the update feed run.ps1 points kuulo at.
param([string]$Root, [int]$Port = 8765)
$listener = New-Object System.Net.HttpListener
$listener.Prefixes.Add("http://localhost:$Port/")
$listener.Start()
while ($listener.IsListening) {
    $context = $listener.GetContext()
    $name = [Uri]::UnescapeDataString($context.Request.Url.AbsolutePath.TrimStart('/'))
    $path = Join-Path $Root $name
    if ($name -and -not $name.Contains('..') -and (Test-Path -LiteralPath $path -PathType Leaf)) {
        $bytes = [IO.File]::ReadAllBytes($path)
        $context.Response.ContentLength64 = $bytes.Length
        $context.Response.OutputStream.Write($bytes, 0, $bytes.Length)
    } else {
        $context.Response.StatusCode = 404
    }
    $context.Response.Close()
}
