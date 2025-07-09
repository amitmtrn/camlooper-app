import { Card } from "@/components/ui/card";
import { Camera, Repeat, Zap, Monitor, Settings, Shield } from "lucide-react";

export const Features = () => {
  const features = [
    {
      icon: Camera,
      title: "Virtual Camera Creation",
      description: "Instantly convert any video recording into a virtual camera source that works with all major applications."
    },
    {
      icon: Repeat,
      title: "Seamless Looping",
      description: "Loop your videos infinitely with smooth transitions. Perfect for backgrounds, demos, or continuous content."
    },
    {
      icon: Zap,
      title: "Lightning Fast",
      description: "Optimized performance ensures minimal system impact while maintaining high-quality video output."
    },
    {
      icon: Monitor,
      title: "Universal Compatibility",
      description: "Works with Zoom, Teams, OBS, Discord, and virtually any application that supports camera input."
    },
    {
      icon: Settings,
      title: "Easy Configuration",
      description: "Simple drag-and-drop interface with customizable loop settings and video quality controls."
    },
    {
      icon: Shield,
      title: "Privacy Focused",
      description: "All processing happens locally on your device. Your videos never leave your computer."
    }
  ];

  return (
    <section id="features" className="py-24 relative">
      <div className="absolute inset-0 bg-gradient-to-b from-background to-background/50" />
      
      <div className="container mx-auto px-4 relative z-10">
        <div className="text-center space-y-4 mb-16">
          <h2 className="text-3xl md:text-5xl font-bold">
            Powerful Features for{" "}
            <span className="bg-gradient-primary bg-clip-text text-transparent">
              Content Creators
            </span>
          </h2>
          <p className="text-xl text-muted-foreground max-w-2xl mx-auto">
            Everything you need to create professional virtual camera setups with ease
          </p>
        </div>

        <div className="grid md:grid-cols-2 lg:grid-cols-3 gap-8">
          {features.map((feature, index) => (
            <Card 
              key={index} 
              className="group p-6 bg-gradient-card border-border hover:border-primary/40 transition-all duration-300 hover:shadow-glow"
            >
              <div className="space-y-4">
                <div className="relative">
                  <div className="w-12 h-12 bg-primary/10 rounded-lg flex items-center justify-center group-hover:bg-primary/20 transition-colors">
                    <feature.icon className="h-6 w-6 text-primary group-hover:scale-110 transition-transform" />
                  </div>
                  <div className="absolute inset-0 bg-primary/20 rounded-lg blur-lg opacity-0 group-hover:opacity-100 transition-opacity" />
                </div>
                
                <h3 className="text-xl font-semibold group-hover:text-primary transition-colors">
                  {feature.title}
                </h3>
                
                <p className="text-muted-foreground leading-relaxed">
                  {feature.description}
                </p>
              </div>
            </Card>
          ))}
        </div>
      </div>
    </section>
  );
};