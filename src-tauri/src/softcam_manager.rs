use anyhow::{anyhow, Result};
use reqwest;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::io::Cursor;
use zip::ZipArchive;

const SOFTCAM_REPO_URL: &str = "https://api.github.com/repos/rdp/screen-capture-recorder-to-video-windows-free/releases/latest";

#[derive(Clone)]
pub struct SoftcamManager {
    downloads_dir: PathBuf,
    dll_x64_path: PathBuf,
    dll_x86_path: PathBuf,
}

impl SoftcamManager {
    pub fn new() -> Result<Self> {
        let downloads_dir = std::env::current_exe()?
            .parent()
            .ok_or_else(|| anyhow!("Cannot get executable directory"))?
            .join("downloads");
        
        // Create downloads directory if it doesn't exist
        std::fs::create_dir_all(&downloads_dir)?;
        
        let dll_x64_path = downloads_dir.join("softcam_x64.dll");
        let dll_x86_path = downloads_dir.join("softcam_x86.dll");
        
        Ok(Self {
            downloads_dir,
            dll_x64_path,
            dll_x86_path,
        })
    }
    
    pub fn is_available(&self) -> bool {
        self.dll_x64_path.exists() || self.dll_x86_path.exists()
    }
    
    pub fn get_dll_path(&self) -> Result<PathBuf> {
        // Prefer x64, fallback to x86
        if self.dll_x64_path.exists() {
            Ok(self.dll_x64_path.clone())
        } else if self.dll_x86_path.exists() {
            Ok(self.dll_x86_path.clone())
        } else {
            Err(anyhow!("No softcam DLL found"))
        }
    }
    
    pub async fn setup_softcam(&self) -> Result<()> {
        println!("Setting up softcam virtual camera...");
        
        // Check if already available
        if self.is_available() {
            println!("Softcam DLL already available");
            return Ok(());
        }
        
        // Get latest release info
        let release_info = self.get_latest_release().await?;
        
        // Download and extract DLLs
        self.download_and_extract_dlls(&release_info).await?;
        
        // Register DLLs
        self.register_dlls().await?;
        
        println!("Softcam setup completed successfully");
        Ok(())
    }
    
    async fn get_latest_release(&self) -> Result<Value> {
        println!("Fetching latest softcam release...");
        
        let client = reqwest::Client::new();
        let response = client
            .get(SOFTCAM_REPO_URL)
            .header("User-Agent", "CamLooper/1.0")
            .send()
            .await?;
        
        if !response.status().is_success() {
            return Err(anyhow!("Failed to fetch release info: {}", response.status()));
        }
        
        let release_info: Value = response.json().await?;
        Ok(release_info)
    }
    
    async fn download_and_extract_dlls(&self, release_info: &Value) -> Result<()> {
        // Find the download URL for the ZIP file
        let assets = release_info["assets"]
            .as_array()
            .ok_or_else(|| anyhow!("No assets found in release"))?;
        
        let zip_asset = assets
            .iter()
            .find(|asset| {
                asset["name"]
                    .as_str()
                    .map(|name| name.ends_with(".zip"))
                    .unwrap_or(false)
            })
            .ok_or_else(|| anyhow!("No ZIP file found in release assets"))?;
        
        let download_url = zip_asset["browser_download_url"]
            .as_str()
            .ok_or_else(|| anyhow!("No download URL found"))?;
        
        println!("Downloading softcam from: {}", download_url);
        
        // Download the ZIP file
        let client = reqwest::Client::new();
        let response = client
            .get(download_url)
            .header("User-Agent", "CamLooper/1.0")
            .send()
            .await?;
        
        if !response.status().is_success() {
            return Err(anyhow!("Failed to download softcam: {}", response.status()));
        }
        
        let zip_content = response.bytes().await?;
        
        // Extract DLLs from the ZIP
        self.extract_dlls_from_zip(&zip_content).await?;
        
        Ok(())
    }
    
    async fn extract_dlls_from_zip(&self, zip_content: &[u8]) -> Result<()> {
        // Use tokio::task::spawn_blocking to handle the synchronous zip extraction
        let dll_x64_path = self.dll_x64_path.clone();
        let dll_x86_path = self.dll_x86_path.clone();
        let zip_content = zip_content.to_vec(); // Clone the data to own it
        
        tokio::task::spawn_blocking(move || {
            let cursor = Cursor::new(zip_content);
            let mut archive = ZipArchive::new(cursor)?;
            
            // Look for DLL files in the archive
            for i in 0..archive.len() {
                let mut file = archive.by_index(i)?;
                let file_name = file.name();
                
                // Check if this is a softcam DLL file
                if file_name.ends_with("softcam.dll") {
                    let target_path = if file_name.contains("x64") || file_name.contains("64") {
                        &dll_x64_path
                    } else if file_name.contains("x86") || file_name.contains("32") {
                        &dll_x86_path
                    } else {
                        // Default to x64 if architecture not specified
                        &dll_x64_path
                    };
                    
                    println!("Extracting {} to {:?}", file_name, target_path);
                    
                    let mut buffer = Vec::new();
                    std::io::copy(&mut file, &mut buffer)?;
                    
                    let mut output_file = std::fs::File::create(target_path)?;
                    std::io::copy(&mut std::io::Cursor::new(buffer), &mut output_file)?;
                }
            }
            
            // If we only found one DLL, copy it to both x64 and x86 paths
            if dll_x64_path.exists() && !dll_x86_path.exists() {
                println!("Copying x64 DLL to x86 path for compatibility");
                std::fs::copy(&dll_x64_path, &dll_x86_path)?;
            } else if !dll_x64_path.exists() && dll_x86_path.exists() {
                println!("Copying x86 DLL to x64 path for compatibility");
                std::fs::copy(&dll_x86_path, &dll_x64_path)?;
            }
            
            Ok::<(), anyhow::Error>(())
        }).await?
    }
    
    async fn register_dlls(&self) -> Result<()> {
        println!("Registering softcam DLL files...");
        
        // Register x64 DLL
        if self.dll_x64_path.exists() {
            match self.register_dll(&self.dll_x64_path).await {
                Ok(_) => println!("Successfully registered 64-bit softcam DLL"),
                Err(e) => println!("Warning: Failed to register 64-bit DLL: {}", e),
            }
        }
        
        // Register x86 DLL
        if self.dll_x86_path.exists() {
            match self.register_dll(&self.dll_x86_path).await {
                Ok(_) => println!("Successfully registered 32-bit softcam DLL"),
                Err(e) => println!("Warning: Failed to register 32-bit DLL: {}", e),
            }
        }
        
        Ok(())
    }
    
    async fn register_dll(&self, dll_path: &Path) -> Result<()> {
        // Use regsvr32 to register the DLL
        let output = Command::new("regsvr32")
            .arg("/s") // Silent mode
            .arg(dll_path)
            .output();
        
        match output {
            Ok(output) => {
                if output.status.success() {
                    Ok(())
                } else {
                    let error_msg = String::from_utf8_lossy(&output.stderr);
                    Err(anyhow!("Failed to register DLL: {}", error_msg))
                }
            }
            Err(e) => {
                // If regsvr32 fails, it might be due to permissions
                // We'll continue anyway and let the user know
                println!("Warning: DLL registration failed (this might require administrator permissions): {}", e);
                Ok(())
            }
        }
    }
    
    /// Unregister softcam DLL files
    pub async fn unregister_dlls(&self) -> Result<()> {
        println!("Unregistering softcam DLL files...");
        
        // Unregister x64 DLL
        if self.dll_x64_path.exists() {
            match self.unregister_dll(&self.dll_x64_path).await {
                Ok(_) => println!("Successfully unregistered 64-bit softcam DLL"),
                Err(e) => println!("Warning: Failed to unregister 64-bit DLL: {}", e),
            }
        }
        
        // Unregister x86 DLL
        if self.dll_x86_path.exists() {
            match self.unregister_dll(&self.dll_x86_path).await {
                Ok(_) => println!("Successfully unregistered 32-bit softcam DLL"),
                Err(e) => println!("Warning: Failed to unregister 32-bit DLL: {}", e),
            }
        }
        
        Ok(())
    }
    
    async fn unregister_dll(&self, dll_path: &Path) -> Result<()> {
        // Use regsvr32 to unregister the DLL
        let output = Command::new("regsvr32")
            .arg("/u") // Unregister
            .arg("/s") // Silent mode
            .arg(dll_path)
            .output();
        
        match output {
            Ok(output) => {
                if output.status.success() {
                    Ok(())
                } else {
                    let error_msg = String::from_utf8_lossy(&output.stderr);
                    Err(anyhow!("Failed to unregister DLL: {}", error_msg))
                }
            }
            Err(e) => {
                println!("Warning: DLL unregistration failed: {}", e);
                Ok(())
            }
        }
    }
    
    /// Get setup instructions for manual installation
    pub fn get_manual_setup_instructions(&self) -> String {
        format!(
            "Manual Softcam Setup Instructions:\n\
            \n\
            1. Download the latest softcam release from:\n\
            {}\n\
            \n\
            2. Extract the softcam.dll files to:\n\
            - 64-bit: {:?}\n\
            - 32-bit: {:?}\n\
            \n\
            3. Register the DLL files by running as Administrator:\n\
            regsvr32 {:?}\n\
            regsvr32 {:?}\n\
            \n\
            4. Restart the application\n\
            \n\
            Note: Administrator privileges may be required for DLL registration.",
            SOFTCAM_REPO_URL,
            self.dll_x64_path,
            self.dll_x86_path,
            self.dll_x64_path,
            self.dll_x86_path
        )
    }
}

// Global instance for easy access
use once_cell::sync::Lazy;
use std::sync::Arc;
use tokio::sync::Mutex;

static SOFTCAM_MANAGER: Lazy<Arc<Mutex<Option<SoftcamManager>>>> = Lazy::new(|| {
    Arc::new(Mutex::new(None))
});

/// Initialize the global softcam manager
pub async fn init_softcam_manager() -> Result<()> {
    let manager = SoftcamManager::new()?;
    let mut global_manager = SOFTCAM_MANAGER.lock().await;
    *global_manager = Some(manager);
    Ok(())
}

/// Get the global softcam manager
pub async fn get_softcam_manager() -> Result<SoftcamManager> {
    let global_manager = SOFTCAM_MANAGER.lock().await;
    global_manager.clone().ok_or_else(|| anyhow!("Softcam manager not initialized"))
}

/// Setup softcam with automatic download and registration
pub async fn setup_softcam() -> Result<()> {
    let manager = get_softcam_manager().await?;
    manager.setup_softcam().await
}

/// Check if softcam is available
pub async fn is_softcam_available() -> bool {
    match get_softcam_manager().await {
        Ok(manager) => manager.is_available(),
        Err(_) => false,
    }
}

/// Get the path to the softcam DLL
pub async fn get_softcam_dll_path() -> Result<PathBuf> {
    let manager = get_softcam_manager().await?;
    manager.get_dll_path()
}

/// Get detailed softcam status information
pub async fn get_softcam_status() -> Result<String> {
    let mut status_info = String::new();
    
    // Check if softcam is available
    let is_available = is_softcam_available().await;
    status_info.push_str(&format!("Softcam Available: {}\n", is_available));
    
    // Try to get DLL path
    match get_softcam_dll_path().await {
        Ok(path) => {
            status_info.push_str(&format!("DLL Path: {:?}\n", path));
            status_info.push_str(&format!("DLL Exists: {}\n", path.exists()));
        }
        Err(e) => {
            status_info.push_str(&format!("DLL Path Error: {}\n", e));
        }
    }
    
    // Check if we can load the DLL
    if is_available {
        status_info.push_str("Status: Ready to use\n");
    } else {
        status_info.push_str("Status: Not available - run setup_softcam to install\n");
    }
    
    Ok(status_info)
} 