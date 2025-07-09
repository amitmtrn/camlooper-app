import { Button } from "@/components/ui/button";
import { Download, Play, Video } from "lucide-react";
import heroImage from "@/assets/hero-image.jpg";

export const Hero = () => {
  return (
    <section className="pt-24 pb-16 min-h-screen flex items-center relative overflow-hidden">
      {/* Background Elements */}
      <div className="absolute inset-0 bg-gradient-hero opacity-50" />
      <div className="absolute top-1/4 left-1/4 w-96 h-96 bg-primary/10 rounded-full blur-3xl animate-float" />
      <div className="absolute bottom-1/4 right-1/4 w-80 h-80 bg-accent/10 rounded-full blur-3xl animate-float" style={{ animationDelay: '1s' }} />
      
      <div className="container mx-auto px-4 relative z-10">
        <div className="grid lg:grid-cols-2 gap-12 items-center">
          {/* Left Content */}
          <div className="space-y-8">
            <div className="space-y-4">
              <div className="inline-flex items-center space-x-2 bg-primary/10 rounded-full px-4 py-2 border border-primary/20">
                <Video className="h-4 w-4 text-primary" />
                <span className="text-sm text-primary font-medium">Virtual Camera Revolution</span>
              </div>
              
              <h1 className="text-4xl md:text-6xl font-bold leading-tight">
                Loop Your Videos with{" "}
                <span className="bg-gradient-primary bg-clip-text text-transparent">
                  Virtual Cameras
                </span>
              </h1>
              
              <p className="text-xl text-muted-foreground leading-relaxed">
                Transform any recording into a seamless virtual camera. Perfect for streaming, 
                video calls, and content creation. Completely free with unobtrusive ads.
              </p>
            </div>

            <div className="flex flex-col sm:flex-row gap-4">
              <Button variant="hero" size="lg" className="group">
                <Download className="h-5 w-5 group-hover:scale-110 transition-transform" />
                Download for Windows
              </Button>
              <Button variant="outline" size="lg" className="group">
                <Play className="h-5 w-5 group-hover:scale-110 transition-transform" />
                Watch Demo
              </Button>
            </div>

            {/* Stats */}
            <div className="flex flex-wrap gap-8 pt-4">
              <div>
                <div className="text-2xl font-bold text-primary">100%</div>
                <div className="text-sm text-muted-foreground">Free Forever</div>
              </div>
              <div>
                <div className="text-2xl font-bold text-primary">50K+</div>
                <div className="text-sm text-muted-foreground">Active Users</div>
              </div>
              <div>
                <div className="text-2xl font-bold text-primary">4.8★</div>
                <div className="text-sm text-muted-foreground">User Rating</div>
              </div>
            </div>
          </div>

          {/* Right Content - Hero Image */}
          <div className="relative">
            <div className="relative rounded-2xl overflow-hidden shadow-card">
              <img
                src={heroImage}
                alt="CamLooper Interface"
                className="w-full h-auto transform hover:scale-105 transition-transform duration-500"
              />
              <div className="absolute inset-0 bg-gradient-primary opacity-10" />
            </div>
            
            {/* Floating Elements */}
            <div className="absolute -top-6 -right-6 bg-card rounded-xl p-4 shadow-card border border-border animate-float">
              <div className="flex items-center space-x-2">
                <div className="w-3 h-3 bg-green-500 rounded-full animate-pulse" />
                <span className="text-sm font-medium">Recording Active</span>
              </div>
            </div>
            
            <div className="absolute -bottom-6 -left-6 bg-card rounded-xl p-4 shadow-card border border-border animate-float" style={{ animationDelay: '1.5s' }}>
              <div className="flex items-center space-x-2">
                <Video className="h-4 w-4 text-accent" />
                <span className="text-sm font-medium">Virtual Cam Ready</span>
              </div>
            </div>
          </div>
        </div>
      </div>
    </section>
  );
};